//! 本机私聊委派的持久接纳、结束和重启恢复。

use super::{
    Back, BotReply, Exchange, MessageLifetime, RequestStatus, check_request, replies, request_prompt, visible_bot,
};
use crate::agent::bot::Cmd;
use crate::hub::Hub;
use crate::store::{BotConfig, Entry, Lane};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

const HOPS: &str = "localBotRequestHops";
const INTERRUPTED: &str = "Interrupted by host restart. Partial work may have happened; check before retrying.";

/// 持久接纳本机请求。双方通知一起落盘后，actor 接收失败也作为已接纳请求的终态告知发送方。
pub(super) fn message_bot(hub: &Arc<Hub>, source: &BotConfig, to: &str, message: &str) -> Result<Value> {
    check_request(hub, source, message)?;
    ensure!(source.id != to, "a bot cannot message itself");
    let target = visible_bot(hub, to)?;
    ensure!(hub.accepts_bot_commands(to), "bot stopped");
    let hops = hub.team.reserve_message(&source.id)?;
    let id = uuid::Uuid::new_v4().to_string();
    let back = Back::Local { sender: source.id.clone() };
    let mut lifetime = MessageLifetime::new(hub, &id, message, back, &target.name);
    let exchange = Exchange { request: &id, source: &source.id, target: to, text: message, peer_name: None };
    let mut sender = exchange.notice_data(&format!("Messaged {}: {message}", target.name));
    let mut recipient = exchange.notice_data(&format!("Message from {}: {message}", source.name));
    sender[HOPS] = hops.into();
    recipient[HOPS] = hops.into();
    lifetime.entries = hub.transcript_update(|| {
        // 回合和通知由同一发布锁排序；恢复标记随双方通知原子提交。
        let notices = [
            (Lane::main(&source.id), hub.store.max_turn(&source.id), sender),
            (Lane::main(to), hub.store.max_turn(to) + 1, recipient),
        ];
        let entries = hub.store.admit_local_request(notices)?;
        Ok((entries.iter().map(|entry| entry.id.clone()).collect(), entries))
    })?;
    let who = format!("Another Codync bot, {},", source.name);
    if let Err(error) = lifetime.deliver(to, hops, request_prompt(&who, &source.name, message)) {
        tracing::warn!(request = id, %error, "recipient refused an admitted bot request");
    }
    Ok(json!({
        "requestId": id, "botId": target.id, "name": target.name, "status": RequestStatus::Queued,
        "note": "Queued. The outcome comes back to you as a new turn in your chat: don't wait or poll for it.",
    }))
}

/// 向接收方发送 prompt 前保存开始边界；缺少接收方通知时不再执行。
pub(super) fn start(message: &MessageLifetime) -> Result<()> {
    let hub = &message.hub;
    hub.transcript_update(|| {
        ensure!(
            message.entries.last().and_then(|id| hub.store.entry(id)).is_some(),
            "the recipient's request is unavailable"
        );
        let changes: Vec<_> = message
            .entries
            .iter()
            .filter_map(|id| {
                // 发送方清理了聊天也不取消此前接纳的请求。
                let mut entry = hub.store.entry(id)?;
                let heading = entry.data["heading"].as_str().unwrap_or("Bot request");
                entry.data["text"] = format!("{heading}\nRunning in {}'s chat…", message.target_name).into();
                entry.data["status"] = json!(RequestStatus::Sent);
                Some((entry.id, entry.data))
            })
            .collect();
        Ok(((), hub.store.update_local_request(&changes, None)?))
    })
}

/// 将双边终态、精确来源及待送回复同事务保存，保存成功后才唤醒发送方。
pub(super) fn finish(
    message: &MessageLifetime,
    sender: &str,
    status: RequestStatus,
    detail: &str,
    preview: Option<&str>,
    source: Option<&replies::ReplySource>,
    outcome: Result<String, String>,
) {
    let hub = &message.hub;
    let result = hub.transcript_update(|| {
        let mut changes = Vec::new();
        let mut pending_entry = None;
        for entry in message.entries.iter().filter_map(|id| hub.store.entry(id)) {
            if entry.bot_id == sender {
                pending_entry = Some(entry.id.clone());
            }
            changes.push((entry.id, replies::finished_data(entry.data, status, detail, preview, source)));
        }
        let reply = pending_entry.map(|entry| BotReply {
            request_id: message.request_id.clone(),
            quote: message.quote.clone(),
            from_name: message.target_name.clone(),
            outcome,
            hops: message.hops,
            pending_entry: Some(entry),
        });
        let serialized = reply.as_ref().map(serde_json::to_string).transpose()?;
        let pending = reply.as_ref().and_then(|r| r.pending_entry.as_deref()).zip(serialized.as_deref());
        let updated = hub.store.update_local_request(&changes, pending)?;
        Ok((reply, updated))
    });
    match result {
        Ok(Some(reply)) => send_reply(hub, sender, reply),
        Ok(None) => {}
        Err(error) => tracing::error!(request = message.request_id, %error, "couldn't save bot request outcome"),
    }
}

/// 通知发送方；关机中的 actor 保留待送正文，已删除的发送方不再需要队列。
fn send_reply(hub: &Hub, sender: &str, reply: BotReply) {
    let entry = reply.pending_entry.clone();
    if let Err(error) = hub.send_cmd(sender, Cmd::BotReply(reply)) {
        if hub.store.bot(sender).is_ok_and(|row| row.is_none_or(|row| row.deleted))
            && let Some(entry) = entry
            && let Err(error) = hub.store.consume_local_replies(&[entry], None)
        {
            tracing::warn!(%error, "couldn't remove a deleted bot's pending reply");
        }
        tracing::debug!(%error, "sender not available for bot reply");
    }
}

/// actor 启动后恢复待送回复，再按原接纳顺序结算本机请求。
pub(crate) fn restore(
    hub: &Arc<Hub>,
    waiting: Vec<Entry>,
    pending_replies: Vec<(String, String, String)>,
) -> Result<()> {
    for (entry, sender, body) in pending_replies {
        match serde_json::from_str::<BotReply>(&body) {
            Ok(reply) if reply.pending_entry.as_deref() == Some(&entry) => send_reply(hub, &sender, reply),
            _ => {
                tracing::warn!(%entry, "couldn't recover a stored bot reply");
                hub.store.consume_local_replies(&[entry], None)?;
            }
        }
    }

    let mut requests: HashMap<String, Vec<Entry>> = HashMap::new();
    for entry in waiting {
        if let Some(id) = entry.data["delegationId"].as_str() {
            requests.entry(id.to_owned()).or_default().push(entry);
        }
    }
    let mut requests: Vec<_> = requests.into_iter().collect();
    requests.sort_by_key(|(_, notices)| notices[0].seq);
    for (id, notices) in requests {
        restore_request(hub, &id, &notices);
    }
    Ok(())
}

/// 只重排已接纳且明确未开始的请求；执行结果未知时通知发送方检查后再重试。
fn restore_request(hub: &Arc<Hub>, id: &str, notices: &[Entry]) {
    let data = &notices[0].data;
    let message = &data["botMessage"];
    let (Some(source), Some(target), Some(text), Some(hops)) = (
        message["sourceBotId"].as_str(),
        message["targetBotId"].as_str(),
        message["text"].as_str(),
        data[HOPS].as_u64().and_then(|hops| u8::try_from(hops).ok()),
    ) else {
        tracing::warn!(request = id, "invalid stored bot request");
        return;
    };
    hub.team.restore_message(source);
    let name = hub.bot_name(target);
    let back = Back::Local { sender: source.to_owned() };
    let mut lifetime = MessageLifetime::new(hub, id, text, back, &name);
    lifetime.entries = notices.iter().map(|entry| entry.id.clone()).collect();
    lifetime.hops = hops;
    let failure = if notices.iter().any(|entry| entry.data["status"] == "sent") {
        Some(INTERRUPTED)
    } else if notices.last().is_none_or(|entry| entry.bot_id != target) || visible_bot(hub, target).is_err() {
        Some("The recipient is unavailable.")
    } else {
        None
    };
    if let Some(detail) = failure {
        lifetime.end(RequestStatus::Failed, detail, None, None, Err(detail.to_owned()));
    } else {
        let name = hub.bot_name(source);
        let who = format!("Another Codync bot, {name},");
        if let Err(error) = lifetime.deliver(target, hops, request_prompt(&who, &name, text)) {
            tracing::warn!(request = id, %error, "couldn't redeliver a waiting bot request");
        }
    }
}
