//! What a user message asks the agent: the note on the message it replies to, its text with a
//! `/skill …` command expanded, then the paths of the files attached to it.

use crate::chat::{replies, uploads};
use crate::hub::Hub;
use crate::store::{BotConfig, Store};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `skills`: the asked bot's skills on. `root`: the chat's uploads (`uploads::root`; a group's
/// own in a group).
/// 将已入库的正文、附件和工作区文件路径一起交给 Agent。
pub fn of_message(store: &Store, skills: &[String], root: &Path, data: &Value) -> String {
    let text = data["text"].as_str().unwrap_or_default();
    let attachments = data["attachments"].as_array().map_or(&[][..], Vec::as_slice);
    let asked = crate::market::expand_command(store, skills, text);
    let asked = asked.as_deref().unwrap_or(text);
    let mut paths: Vec<PathBuf> = attachments.iter().filter_map(|meta| uploads::path_of(root, meta)).collect();
    // 工作区文件使用发送时核验过的主机路径，群成员的当前目录可能不同。
    paths.extend(
        data["workspaceFilePaths"].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from),
    );
    if paths.is_empty() { asked.to_owned() } else { format!("{asked}{}", uploads::prompt_suffix(&paths)) }
}

/// What a user message in `bot`'s own chat (its saved `data`) asks the agent.
pub fn of_entry(hub: &Hub, bot: &BotConfig, data: &Value) -> String {
    let asked = of_message(&hub.store, &bot.skills, &uploads::root(bot), data);
    match replies::note(hub, &bot.id, &data["replyTo"]) {
        Some(note) => format!("{note}\n{asked}"),
        None => asked,
    }
}
