//! The settings the other apps keep next to the chat, as TUI sheets: a bot's memory and
//! routines, the marketplace (agents, connectors, skills), agent sign-in with its setup
//! terminal, and the pieces of a message you can act on (reactions, sent files).

// Key handlers read best with k (key), m / l / f / a (the sheet), n (rows).
#![allow(clippy::many_single_char_names)]

use super::net::files::SharedFileAction;

mod agent;
mod analytics;
mod bot_chat;
mod dev_servers;
mod fields;
mod gear;
mod icons;
mod library;
mod market;
mod memory;
mod network;
mod network_rules;
mod network_setup;
mod pages;
mod replies;
mod routines;
mod skill;
mod when;

pub use analytics::AnalyticsSheet;
pub use dev_servers::{DevServer, DevServerSummary, DevServersSheet, LogsView, summary as dev_server_summary};
pub use fields::{handoff_fields, section_fields};
pub use gear::{Gear, GearRow};
pub use icons::icon_fields;
pub use library::{ArtifactTitles, LibraryItem};
pub use memory::{MemoryMode, MemorySheet, Scroll, Section};
pub use network::{NetReply, NetworkSheet, Recovery, Step};
pub use network_rules::{RuleField, RuleForm};
pub use network_setup::{CONNECTORS, Setup, SetupField};
pub use pages::Page;
pub use skill::SkillSheet;
pub use when::complete_zone;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

use std::cell::Cell;

use super::app::{After, App, Editor, Entry};

/// Quick reactions, as the other apps offer them.
pub const REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "👀", "✅"];

/// Replies to the sheets' commands.
#[derive(Clone)]
pub enum Reply {
    /// A dev server's log, by the server's id.
    DevServerLogs(String),
    Memory(String),
    MemorySaved(String),
    /// A note the bot's memory sheet has open.
    MemoryFile(String),
    Routines(String),
    BotSessionUsage(String),
    PrepareHandoff(String),
    GroupHandoff(String),
    MemberSession {
        room: String,
        bot: String,
    },
    RoutineSaved,
    /// A bot's Library (`library {botId}`).
    Library(String),
    /// What an agent brings on its own (`agentGear`), for its sign-in sheet.
    Gear(String),
    /// A routine's webhook URL and key: copied as a ready-to-run curl.
    Webhook,
    /// A bot conversation's fetched history.
    BotChat {
        bot: String,
        peer: String,
    },
    /// A voice call's own messages, by call id.
    CallEntries(String),
    /// A registry page: the search it lists and the cursor it was asked at (`None`: the first page).
    Registry {
        search: String,
        cursor: Option<String>,
    },
    Skills,
    /// A skill's instructions, for the details sheet with that id.
    SkillDetails(String),
    Auth(String),
    Setup,
    FieldsSaved,
    /// `renameSection` from a Fields sheet: saved like `FieldsSaved`, and the fold follows the name.
    SectionRenamed {
        from: String,
        to: Option<String>,
    },
    SignIn,
    Models(String),
    Downloaded,
    /// Something changed: every open sheet loads again.
    Changed,
}

/// A read-only conversation between `bot` and `peer`, scrolled from the bottom.
pub struct BotChatSheet {
    pub bot: String,
    pub peer: String,
    /// The host's history; `None` while it loads.
    pub fetched: Option<Vec<Entry>>,
    /// Lines up from the bottom.
    pub scroll: usize,
    /// Set by the last draw, for clamping the scroll and paging.
    pub max_scroll: Cell<usize>,
    pub page: Cell<usize>,
}

/// A bot's Library tab: its artifacts and the files shared in its chat, newest first.
pub struct Library {
    pub bot: String,
    pub items: Option<Vec<LibraryItem>>,
    pub cursor: usize,
}

pub struct RoutineList {
    pub bot: String,
    pub session_usage: Value,
    pub items: Option<Vec<Value>>,
    /// The bot's runs, newest first.
    pub runs: Vec<Value>,
    pub cursor: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoutineField {
    Name,
    Instruction,
    When,
}

pub struct RoutineForm {
    pub bot: String,
    pub id: Option<String>,
    pub name: Editor,
    pub instruction: Editor,
    pub when: Editor,
    /// What an edited routine runs on; kept when `when` stays empty.
    pub original: Value,
    pub original_text: String,
    pub field: RoutineField,
    pub error: Option<String>,
    pub saving: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Agents,
    Connectors,
    Skills,
}

pub const TABS: [(Tab, &str); 3] = [(Tab::Agents, "Agents"), (Tab::Connectors, "Connectors"), (Tab::Skills, "Skills")];

pub struct Market {
    pub tab: Tab,
    pub query: Editor,
    /// The registry search the connector list shows; `None` before the first page.
    pub searched: Option<String>,
    pub registry: Vec<Value>,
    pub next: Option<String>,
    pub skills: Option<Vec<Value>>,
    pub loading: bool,
    pub cursor: usize,
    pub error: Option<String>,
}

pub enum Row<'a> {
    /// Some agents run on Node.js and the computer has none: adds Codync's copy.
    Node,
    Agent(&'a Value),
    Installed(&'a Value),
    Custom,
    Registry(&'a Value),
    OwnSkill(&'a Value),
    Skill(&'a Value),
}

pub struct AgentSetup {
    pub backend: String,
    pub auth: Option<Value>,
    /// A check or a browser sign-in is running.
    pub busy: bool,
    pub cursor: usize,
    pub error: Option<String>,
}

pub enum SetupRow {
    Install,
    Node,
    Login,
    Method(Value),
    Check,
    /// Opens the read-only lists of what this agent already brings.
    Brings,
}

pub struct Input {
    pub name: String,
    pub label: String,
    pub secret: bool,
    pub required: bool,
    pub multiline: bool,
    pub placeholder: String,
    pub ed: Editor,
}

pub enum Submit {
    Icon(String),
    AgentEnv(String),
    Connection(Box<super::connections::Connection>),
    Connector(Value),
    Import,
    /// A handoff card's text (the card's entry id).
    Handoff(String),
    GroupHandoff {
        delivery: String,
        revision: String,
    },
    MemberSession {
        room: String,
        bot: String,
        revision: String,
    },
    GroupContextOptions {
        room: String,
        members: Vec<(String, bool)>,
    },
    /// A bot's section (its id): a name, or empty for none.
    Section(String),
    /// A section's new name (its current one), or empty to remove it.
    RenameSection(String),
    RenameArtifact {
        bot: String,
        artifact: String,
    },
}

/// A few labeled text fields: an agent's keys, a connector's setup, a pasted MCP config.
pub struct Fields {
    pub title: String,
    pub note: String,
    /// A connector's ways to run (row 0 when there are several).
    pub choices: Vec<String>,
    pub choice: usize,
    pub inputs: Vec<Input>,
    pub cursor: usize,
    pub error: Option<String>,
    pub saving: bool,
    pub submit: Submit,
}

impl Fields {
    fn offset(&self) -> usize {
        usize::from(self.choices.len() > 1)
    }

    pub fn current(&mut self) -> Option<&mut Input> {
        let i = self.cursor.checked_sub(self.offset())?;
        self.inputs.get_mut(i)
    }
}

/// The running setup terminal, which takes over the whole screen.
pub struct Shell {
    pub id: String,
    /// Keys in the order typed (one sender, one request at a time).
    pub input: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pub backend: String,
    pub exited: Option<i64>,
    /// Output not yet written to the screen.
    pub out: Vec<u8>,
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

fn ctrl(k: KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
}

fn newline(k: KeyEvent) -> bool {
    k.code == KeyCode::Enter && k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        || k.code == KeyCode::Char('j') && ctrl(k)
}

impl App {
    fn sheet(&self, method: &'static str, body: Value, r: Reply) {
        self.call(method, body, After::Sheet(r));
    }

    /// Loads what voice call `call` in `bot`'s chat carried, for its unfolded conversation.
    pub(in crate::tui) fn fetch_call(&self, bot: &str, call: &str) {
        self.sheet("callEntries", json!({"botId": bot, "callId": call}), Reply::CallEntries(call.to_owned()));
    }

    // ---------- a message's actions ----------

    pub(super) fn react(&mut self, entry: &str, i: usize) {
        if let Some(e) = REACTIONS.get(i) {
            self.call("react", json!({"entryId": entry, "emoji": e}), After::Nothing);
        }
    }

    /// `f` while a download runs: stops it.
    fn cancel_download(&mut self) -> bool {
        let running = self.download.take().is_some();
        if running {
            self.flash("Download cancelled · f retries");
        }
        running
    }

    /// Saves a message's files, or its artifact, to Downloads.
    pub(super) fn save_files(&mut self, bot: &str, entry: &str, data: &Value) {
        if self.cancel_download() {
            return;
        }
        if let Some(artifact) = data["artifact"]["id"].as_str() {
            self.fetch_artifact(bot, artifact, super::net::artifacts::ArtifactTo::Downloads);
            return;
        }
        if let Some(meta) = data["files"].as_array().and_then(|files| files.first()) {
            match serde_json::from_value(meta.clone()) {
                Ok(file) => self.download_shared(entry.to_owned(), file, SharedFileAction::Save),
                Err(error) => self.flash(&format!("Invalid file: {error}")),
            }
            return;
        }
        let files: Vec<(String, String)> = data["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| Some((a["id"].as_str()?.to_owned(), a["name"].as_str()?.to_owned())))
            .collect();
        if files.is_empty() {
            self.flash("No files on that message");
            return;
        }
        self.save_uploads(bot, files);
    }

    /// Saves a file the bot shared (`readFile`), with progress; `f` again cancels.
    fn download_shared(&mut self, entry: String, file: crate::chat::files::SharedFile, action: SharedFileAction) {
        self.download = Some(self.client.download_file(entry, file, action, self.tx.clone()));
        self.flash("Downloading… · f cancels");
    }

    /// Saves files the user attached (`(upload id, name)`, `readUpload`).
    fn save_uploads(&mut self, bot: &str, files: Vec<(String, String)>) {
        self.flash("Saving…");
        self.client.spawn_download(bot.to_owned(), files, self.tx.clone());
    }

    // ---------- model picker ----------

    /// Loads the models `backend` offers, once per run.
    pub(super) fn want_models(&mut self, backend: &str) {
        if !backend.is_empty() && !self.models.contains_key(backend) {
            self.models.insert(backend.to_owned(), None);
            self.sheet("agentModels", json!({"backend": backend}), Reply::Models(backend.to_owned()));
        }
    }

    /// ←→ on the model field: "default", then each advertised model.
    pub(super) fn cycle_model(&self, backend: &str, current: &str, d: isize) -> Option<String> {
        let list = self.models.get(backend)?.as_ref()?;
        let mut ids = vec![String::new()];
        ids.extend(list.iter().filter(|m| !m.id.is_empty()).map(|m| m.id.clone()));
        let i = ids.iter().position(|x| x == current).unwrap_or(0);
        Some(ids[super::app::cycle(i, d, ids.len())].clone())
    }

    /// The settings (effort, fast…) `backend`'s `model` offers.
    pub(crate) fn model_settings(&self, backend: &str, model: &str) -> &[super::app::ModelOption] {
        let list = self.models.get(backend).and_then(Option::as_ref);
        list.and_then(|l| l.iter().find(|m| m.id == model)).map_or(&[], |m| &m.options)
    }
}

/// Opens `url` in this computer's browser, when the host is this computer.
pub(super) fn open_browser(url: &str, host: &str) -> bool {
    let local = ["//127.0.0.1", "//localhost", "//[::1]"].iter().any(|h| host.contains(h));
    local && !url.is_empty() && crate::shell::open(std::ffi::OsStr::new(url))
}

/// 比例以整数舍入到一位小数，避免放大 Token 计数或浮点精度损失。
fn context_percent(used: u64, capacity: u64) -> String {
    let capacity = u128::from(capacity.max(1));
    let tenths = (u128::from(used) * 1000 + capacity / 2) / capacity;
    format!("{}.{:01}%", tenths / 10, tenths % 10)
}

/// 当前选择的基准只控制显示，超过档位的比例原样显示。
pub(in crate::tui) fn context_reading(member: &Value, basis: u64) -> String {
    member["used"].as_u64().map_or_else(
        || {
            super::l10n::tr(if member["hasSession"] == false { "No session yet" } else { "Usage not reported" })
                .into_owned()
        },
        |used| {
            let tenths = (u128::from(used) + 50) / 100;
            format!("{}.{:01}K · {}", tenths / 10, tenths % 10, context_percent(used, basis))
        },
    )
}

/// 只有待确认派工或真实窗口达到阈值才有主动入口。
pub(in crate::tui) fn context_action(member: &Value) -> bool {
    match member["handoff"]["phase"].as_str() {
        Some("draft") => true,
        Some("approved" | "switched") => false,
        _ => member["recommended"] == true,
    }
}

impl App {
    /// 键盘也能浏览全部成员及实际窗口，并选择显示档位。
    pub(in crate::tui) fn open_group_contexts(&mut self) {
        let Some((room, snapshot)) =
            self.group_contexts.as_ref().filter(|(room, _)| self.selected.as_ref() == Some(room))
        else {
            self.flash("Loading member context…");
            return;
        };
        let basis = snapshot["basis"].as_u64().unwrap_or(512_000);
        let mut choices: Vec<String> = ["256K", "512K", "1M"].into_iter().map(str::to_owned).collect();
        let mut members = Vec::new();
        for reading in snapshot["members"].as_array().into_iter().flatten() {
            let Some(bot) = reading["botId"].as_str() else { continue };
            let name = self.author_name(Some(bot));
            let reading_text = context_reading(reading, basis);
            choices.push(format!("{name} · {reading_text}"));
            members.push((bot.to_owned(), false));
            if context_action(reading) {
                choices.push(format!("{} · {name}", super::l10n::tr("Open new session")));
                members.push((bot.to_owned(), true));
            }
        }
        self.overlays.push(super::app::Overlay::Fields(Box::new(Fields {
            title: "Member context".into(),
            note: "← → choose a display basis or inspect a member. ↵ confirms.".into(),
            choices,
            choice: [256_000, 512_000, 1_000_000].iter().position(|&size| size == basis).unwrap_or(1),
            inputs: Vec::new(),
            cursor: 0,
            error: None,
            saving: false,
            submit: Submit::GroupContextOptions { room: room.clone(), members },
        })));
    }

    /// 查看任何成员的精确用量和真实窗口，不受推荐阈值影响。
    pub(in crate::tui) fn inspect_member_context(&mut self, room: &str, bot: &str) {
        let reading = self
            .group_contexts
            .as_ref()
            .filter(|(chat, _)| chat == room)
            .and_then(|(_, snapshot)| snapshot["members"].as_array())
            .and_then(|members| members.iter().find(|row| row["botId"] == bot));
        if let Some(reading) = reading {
            let used = reading["used"].as_u64();
            let cap = reading["size"].as_u64();
            let occupancy = used.zip(cap).map_or_else(|| "—".into(), |(n, size)| context_percent(n, size));
            let name = self.author_name(Some(bot));
            self.flash(&format!(
                "{name} · {} token · {} {} · {occupancy}",
                used.map_or_else(|| "—".into(), |n| n.to_string()),
                super::l10n::tr("Actual model window"),
                cap.map_or_else(|| "—".into(), |n| n.to_string())
            ));
        }
    }

    /// 从成员卡准备交接，普通群在服务端只返回私聊定位。
    pub(in crate::tui) fn open_member_session(&mut self, room: String, bot: String) {
        let action = self
            .group_contexts
            .as_ref()
            .filter(|(chat, _)| chat == &room)
            .and_then(|(_, snapshot)| snapshot["members"].as_array())
            .and_then(|members| members.iter().find(|m| m["botId"] == bot))
            .is_some_and(context_action);
        if action {
            self.sheet(
                "prepareGroupMemberSession",
                json!({"roomId":room,"botId":bot}),
                Reply::MemberSession { room, bot },
            );
        }
    }
}
