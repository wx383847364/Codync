//! The user's messages in a chat: typed (`send`) or relayed from a voice call (`chat::calls`).

use crate::agent::bot::Cmd;
use crate::hub::Hub;
use crate::store::{BotConfig, Entry, EntryKind, Lane};
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::sync::Arc;

/// Posts `data` (the message's text and what it carries) as the user's newest message in `cfg`'s chat and hands it
/// on: to the bot's agent, or to a group's members.
/// 入库时固定群消息目标，正文、身份与投递原子提交后才唤醒调度。
pub fn post(hub: &Arc<Hub>, cfg: &BotConfig, mut data: Value) -> Result<Entry> {
    // A group has no agent of its own to queue behind: its message is simply sent.
    data["status"] = (if cfg.is_group() { "sent" } else { "queued" }).into();
    let lane = Lane::main(&cfg.id);
    let turn = hub.store.max_turn(&cfg.id) + 1;
    // 新旧客户端统一在入库时解析；保存后不再按后来改名重新路由。
    let e = if cfg.is_group() {
        let members = cfg
            .members
            .iter()
            .map(|id| hub.store.bot(id))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .filter(|r| !r.deleted)
            .map(|r| r.config)
            .collect::<Vec<_>>();
        let mentions: Vec<crate::chat::mentions::Mention> =
            data.get("mentions").cloned().map(serde_json::from_value).transpose()?.unwrap_or_default();
        let targets = crate::chat::mentions::targets(
            data["text"].as_str().unwrap_or_default(),
            &mentions,
            &members,
            data["replyTo"]["author"].as_str(),
            true,
        )?;
        data["resolvedMentions"] = serde_json::json!(targets);
        hub.post_group(&cfg.id, &data, &targets)?
    } else {
        hub.add_entry(&lane, EntryKind::User, turn, &data).ok_or_else(|| anyhow!("couldn't save the message"))?
    };
    // Before the turn starts, so its reply lands unread.
    if let Err(error) = hub.mark_read(&cfg.id) {
        tracing::warn!(%error, bot = %cfg.id, "couldn't mark bot read after send");
    }
    if cfg.is_group() {
        // Each member reads it in its room prompt (`chat::group`).
        crate::chat::group::start(hub, cfg, lane);
    } else {
        // `/skill …` reaches the agent as "read this skill first"; the chat keeps what was typed.
        let prompt = crate::chat::prompt::of_entry(hub, cfg, &e.data);
        hub.send_cmd(&cfg.id, Cmd::Send { lane, entry_id: e.id.clone(), text: prompt })?;
    }
    Ok(e)
}
