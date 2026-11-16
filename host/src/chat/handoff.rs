//! A rollover's handoff. When the bot's main conversation is due to start over (see `agent::bot::compaction`), a
//! card in its chat shows what the new conversation will carry; the user can edit it, or keep the old conversation,
//! until their next message starts the new one with it. The card is a notice whose `handoff` field holds the draft,
//! so an app that doesn't know it still shows the text.

use crate::hub::Hub;
use crate::store::{BotConfig, Entry, EntryKind, Lane, Store};
use anyhow::{Result, anyhow, bail};
use codync_mem::compaction::{Handoff, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The longest handoff a user can save.
const MAX_CHARS: usize = 20_000;
/// The user's messages a handoff repeats word for word.
const REQUESTS: usize = 4;
/// Main-chat entries read to build one.
const READ: i64 = 120;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// 等待下一条私聊消息，或在容量提醒中等待明确确认。
    Draft,
    /// 用户已确认以新会话接续排队消息。
    Approved,
    Sent,
    /// 启动失败，卡片仍可查看原交接摘要。
    Failed,
    /// The user kept the old conversation.
    Kept,
}

/// Why the conversation starts over.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// A large conversation whose prompt cache expired: going on would read it all again at full price.
    Cold,
    /// A long absence after several summaries.
    Away,
    /// Summaries stopped freeing room.
    Full,
    /// 旧会话剩余空间预计不足，需要用户选择如何执行已排队的任务。
    Capacity,
    /// 用户在机器人详情主动准备新会话。
    Requested,
}

impl Reason {
    /// 给交接卡提供与触发原因一致的操作说明。
    fn line(self) -> &'static str {
        match self {
            Self::Cold => {
                "This conversation is long and has gone quiet, so your next message starts a fresh one carrying this summary."
            }
            Self::Away => {
                "You've been away a while and this conversation has been summarized several times, so your next message starts a fresh one carrying this summary."
            }
            Self::Capacity => {
                "This task may exceed the available context. Review what will carry over, then start a new session with the button or /new, or try the current session."
            }
            Self::Requested => "Your next message starts a fresh session carrying this summary.",
            Self::Full => {
                "Summarizing this conversation no longer frees enough room, so your next message starts a fresh one carrying this summary."
            }
        }
    }
}

fn key(bot: &str) -> String {
    format!("handoff.{bot}")
}

/// Where the harness's last summary of a session is kept.
pub fn summary_key(bot: &str, session: &str) -> String {
    format!("compaction.summary.{bot}.{session}")
}

/// Posts the card for `session`, the bot's main conversation, built from its chat.
/// 容量提醒的首条待执行消息是恢复锚点，交接摘要只读取它之前的聊天。
pub fn draft(hub: &Hub, cfg: &BotConfig, session: &str, reason: Reason, first_queued: Option<i64>) -> Result<Entry> {
    let text = build(&hub.store, &cfg.id, session, first_queued)?;
    let lane = Lane::main(&cfg.id);
    let data = json!({
        "text": reason.line(),
        "style": "info",
        "author": cfg.id,
        "handoff": {"status": Status::Draft, "reason": reason, "text": text, "firstQueuedSeq": first_queued},
    });
    let entry = hub
        .add_entry(&lane, EntryKind::Notice, hub.store.max_turn(&cfg.id), &data)
        .ok_or_else(|| anyhow!("couldn't post the handoff"))?;
    hub.store.kv_set(&key(&cfg.id), &entry.id)?;
    Ok(entry)
}

/// 从聊天检查点生成简短交接文本，供用户查看和编辑。
pub(crate) fn build(store: &Store, bot: &str, session: &str, first_queued: Option<i64>) -> Result<String> {
    // 任务可能在前一轮执行时提交，生成时已落地的回复和计划仍需要进入交接。
    let entries = store.history(bot, i64::MAX, READ)?;
    let included = |entry: &&Entry| {
        first_queued.is_none_or(|seq| {
            entry.kind != EntryKind::User.as_str() || (entry.seq < seq && entry.data["status"] != "queued")
        })
    };
    let user = EntryKind::User.as_str();
    let requests: Vec<(String, String)> = entries
        .iter()
        .filter(included)
        .filter(|e| e.kind == user)
        .rev()
        .take(REQUESTS)
        .map(|e| (crate::chat::memory::ymd(e.created_at), e.data["text"].as_str().unwrap_or_default().to_owned()))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let requests: Vec<Request> = requests.iter().map(|(date, text)| Request { date, text }).collect();
    let reply = entries.iter().rev().find(|e| e.kind == "agent" && e.data["final"] == true);
    let plan = entries.iter().rev().find(|e| e.kind == "plan").map(|e| plan_text(&e.data));
    let summary = store.kv_get(&summary_key(bot, session));
    let bookmark = entries
        .iter()
        .filter(included)
        .rev()
        .find(|e| e.kind == user || (e.kind == "agent" && e.data["final"] == true));
    Ok(Handoff {
        summary: summary.as_deref(),
        requests: &requests,
        reply: reply.and_then(|e| e.data["text"].as_str()),
        plan: plan.as_deref(),
        bookmark: bookmark.map(|e| e.id.as_str()),
    }
    .render())
}

pub(crate) fn plan_text(data: &Value) -> String {
    let steps = data["entries"].as_array().into_iter().flatten();
    steps
        .map(|s| {
            format!("- [{}] {}", s["status"].as_str().unwrap_or("pending"), s["content"].as_str().unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 读取当前交接卡，旧卡不能改变新的待执行任务。
pub(crate) fn current(store: &Store, bot: &str) -> Option<Entry> {
    store.kv_get(&key(bot)).filter(|id| !id.is_empty()).and_then(|id| store.entry(&id))
}

/// 读取尚未处理的交接，等待用户确认的卡也属于当前交接。
pub fn pending(store: &Store, bot: &str) -> Option<(String, String)> {
    let entry = current(store, bot)?;
    let card = &entry.data["handoff"];
    matches!(card["status"].as_str(), Some("draft" | "approved"))
        .then(|| (entry.id, card["text"].as_str().unwrap_or_default().to_owned()))
}

/// 容量提醒必须经用户确认，普通交接仍由下一条私聊消息接续。
pub(crate) fn awaits_confirmation(entry: &Entry) -> bool {
    entry.data["handoff"]["reason"] == json!(Reason::Capacity)
        && entry.data["handoff"]["status"] == json!(Status::Draft)
}

/// 旧客户端通过 /new 确认容量交接，重复请求也应保留已确认的摘要。
pub(crate) fn approve_from_session_switch(hub: &Hub, bot: &str) -> bool {
    let Some(card) = current(&hub.store, bot) else { return false };
    if card.data["handoff"]["reason"] != json!(Reason::Capacity) {
        return false;
    }
    if awaits_confirmation(&card)
        && let Err(error) = call(hub, "startHandoff", &json!({"entryId": card.id}))
    {
        tracing::warn!(%error, "couldn't approve the handoff");
    }
    matches!(card.data["handoff"]["status"].as_str(), Some("draft" | "approved"))
}

/// 用户选择沿用旧会话；处理后的交接不再影响后续任务。
pub fn kept(store: &Store, bot: &str) -> bool {
    let Some(entry) = current(store, bot) else { return false };
    let status = entry.data["handoff"]["status"].as_str();
    if !matches!(status, Some("draft" | "approved")) {
        let _ = store.kv_delete(&key(bot));
    }
    status == Some("kept")
}

/// 手动切换或更换模型会使旧会话的未确认交接失效。
pub(crate) fn clear(hub: &Hub, bot: &str) {
    if let Some(entry) = current(&hub.store, bot) {
        if pending(&hub.store, bot).is_some() {
            set_status(hub, &entry.id, Status::Kept, "The handoff is no longer pending.");
        }
        let _ = hub.store.kv_delete(&key(bot));
    }
}

/// ACP 提交完成后才宣告交接已使用，启动失败仍保留摘要供查看。
pub fn finish(hub: &Hub, bot: &str, id: &str, submitted: bool) {
    let (status, line) = if submitted {
        (Status::Sent, "Started a fresh conversation with this summary.")
    } else {
        (Status::Failed, "The task did not start with this summary.")
    };
    set_status(hub, id, status, line);
    let _ = hub.store.kv_delete(&key(bot));
}

fn set_status(hub: &Hub, id: &str, status: Status, line: &str) -> Option<Entry> {
    let mut data = hub.store.entry(id)?.data;
    data["handoff"]["status"] = json!(status);
    data["text"] = line.into();
    hub.set_entry(id, &data)
}

/// 编辑或确认当前交接，容量提醒经确认后才会放行待执行消息。
pub fn call(hub: &Hub, method: &str, b: &Value) -> Result<Value> {
    let id = b["entryId"].as_str().ok_or_else(|| anyhow!("entryId required"))?;
    let entry = hub.store.entry(id).filter(|e| e.data["handoff"].is_object()).ok_or_else(|| anyhow!("no such card"))?;
    if let Some(room) = entry.data["memberSessionRoom"].as_str() {
        anyhow::ensure!(matches!(method, "editHandoff" | "startHandoff"), "Review the handoff in its own chat");
        let text = (method == "editHandoff")
            .then(|| b["text"].as_str().ok_or_else(|| anyhow!("text required")))
            .transpose()?;
        let revision = entry.data["handoff"]["revision"].as_str().unwrap_or_default();
        hub.group_handoff_update(|| {
            Ok((
                (),
                hub.store.decide_member_session(room, &entry.bot_id, true, revision, method == "startHandoff", text)?,
            ))
        })?;
        return Ok(json!({"entry":hub.store.entry(id)}));
    }
    if let Some(delivery) = entry.data["groupDeliveryId"].as_str() {
        anyhow::ensure!(matches!(method, "editHandoff" | "startHandoff"), "请在原派工继续操作");
        let text = (method == "editHandoff")
            .then(|| b["text"].as_str().ok_or_else(|| anyhow!("text required")))
            .transpose()?;
        let revision = entry.data["handoff"]["revision"].as_str().unwrap_or_default();
        hub.group_handoff_update(|| {
            Ok(((), hub.store.decide_group_handoff(delivery, true, method == "startHandoff", revision, text)?))
        })?;
        return Ok(json!({"entry":hub.store.entry(id)}));
    }
    if method == "startHandoff" && matches!(entry.data["handoff"]["status"].as_str(), Some("approved" | "sent")) {
        return Ok(json!({"entry": entry}));
    }
    if entry.data["handoff"]["status"] != json!(Status::Draft)
        || current(&hub.store, &entry.bot_id).is_none_or(|current| current.id != id)
    {
        bail!("This conversation already moved on.");
    }
    let entry = match method {
        "editHandoff" => {
            let text =
                b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| anyhow!("text required"))?;
            if text.chars().count() > MAX_CHARS {
                bail!("A handoff can be at most {MAX_CHARS} characters.");
            }
            let mut data = entry.data;
            data["handoff"]["text"] = text.into();
            hub.set_entry(id, &data)
        }
        "keepConversation" => set_status(hub, id, Status::Kept, "Kept this conversation."),
        "startHandoff" if awaits_confirmation(&entry) => {
            set_status(hub, id, Status::Approved, "Opening a new session with this summary.")
        }
        _ => bail!("unknown method {method}"),
    };
    Ok(json!({"entry": entry.ok_or_else(|| anyhow!("couldn't save the handoff"))?}))
}
