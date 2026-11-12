//! What reaches a bot's actor ([`Cmd`]) and the turns that wait in its queue ([`Queued`]).

use crate::store::{BotConfig, Lane};
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;

pub enum Cmd {
    RefreshTools,
    Routine(String),
    /// The user wrote in the bot's chat.
    Send {
        lane: Lane,
        entry_id: String,
        text: String,
    },
    /// The user wants a queued message of theirs handled now (`steer`).
    Steer {
        entry_id: String,
    },
    /// What the agent made of a steered message (`_session/steering`).
    Steered {
        entry_id: String,
        outcome: Result<Value>,
    },
    /// The user took back a queued message of theirs (`unqueue`).
    Unqueue {
        entry_id: String,
    },
    BotRequest(crate::chat::team::BotRequest),
    /// How a request this bot sent came out: a turn of its own, with any others that arrive meanwhile.
    BotReply(crate::chat::team::BotReply),
    /// A turn the host starts in the bot's own chat, with no message of the user's (the lead opening a call): its
    /// reply is that call's (`chat::calls`).
    Prompt {
        text: String,
        call: Option<String>,
    },
    /// `send_message` from the bot's `chat` MCP server: a message for the user, now.
    SendToUser {
        group_session: Option<String>,
        text: String,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// `react` from the bot's `chat` MCP server: an emoji on the message the turn answers.
    React {
        group_session: Option<String>,
        emoji: String,
        /// In a room: whose message (see `chat::reactions::in_room`).
        to: Option<String>,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
    SendFile {
        group_session: Option<String>,
        path: String,
        name: Option<String>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// An artifact the bot just published (`chat::artifacts`): its card (the entry's `data`) in the turn's chat. Answers
    /// the card's entry id.
    PostArtifact {
        group_session: Option<String>,
        data: Value,
        reply: tokio::sync::oneshot::Sender<Result<String>>,
    },
    /// 只停止本次已持久取消的投递；延迟命令不能影响同群后来接受的新工作。
    CancelGroup {
        chat: String,
        delivery_ids: Vec<String>,
    },
    Stop,
    Permission {
        entry_id: String,
        option_id: Option<String>,
    },
    /// A Codync tool asks the user before it acts (`chat::manage`), on the agent's kind of approval card.
    Approve {
        group_session: Option<String>,
        title: String,
        detail: String,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// 只领取房间 runner 指定的持久投递，reply 只表示等待结束。
    Delivery {
        id: String,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    RetryDeliverySave,
    SessionSwitch {
        mode: String,
    },
    Reconfigure(Box<BotConfig>),
    /// The user wants the bot's own agent again (`switchBack`).
    SwitchBack,
    /// A voice call ended (`chat::calls`): its replies show in the chat again, and dreaming reads it.
    CallEnded {
        call: String,
    },
    Shutdown,
}

pub(super) enum Queued {
    Routine(String),
    User {
        lane: Lane,
        entry_id: String,
        text: String,
    },
    BotRequest(crate::chat::team::BotRequest),
    BotReply(crate::chat::team::BotReply),
    Prompt {
        text: String,
        call: Option<String>,
    },
    /// A compaction the host decided on (see `compaction`); `idle` while the user is away.
    Compact {
        idle: bool,
        group: Option<super::group_context::Maintenance>,
    },
}
