//! 群成员活动及旧会话的终端导航和确认。

use crossterm::event::KeyCode;
use serde_json::{Value, json};

use super::super::app::{App, Confirm, ConfirmAct, Overlay};
use super::Reply;

impl App {
    /// 创建历史弹层并请求对应作用域的当前、旧会话。
    pub(in crate::tui) fn open_session_history(&mut self, bot: String, room: Option<String>) {
        self.sheet(
            "sessionHistory",
            json!({"botId":bot,"roomId":room}),
            Reply::SessionHistory { bot: bot.clone(), room: room.clone() },
        );
        self.overlays.push(Overlay::SessionHistory { bot, room, snapshot: Value::Null, messages: None, cursor: 0 });
    }

    /// 群套件的成员入口从 Host 读取最近派工，兼容尚未载入的历史。
    pub(in crate::tui) fn open_member_activity(&mut self, room: String, bot: String) {
        self.sheet(
            "groupMemberActivity",
            json!({"botId":bot,"roomId":room}),
            Reply::MemberActivity { bot: bot.clone(), room: room.clone() },
        );
        self.overlays.push(Overlay::MemberActivity { bot, room, snapshot: Value::Null, result: None, cursor: 0 });
    }

    /// 导航历史条目或预览文本，删除需经过终端确认。
    pub(in crate::tui) fn session_history_key(
        &mut self,
        bot: String,
        room: Option<String>,
        snapshot: Value,
        mut messages: Option<Value>,
        mut cursor: usize,
        key: KeyCode,
    ) -> Option<Overlay> {
        let sessions = snapshot["sessions"].as_array().map(Vec::as_slice).unwrap_or_default();
        let current = sessions.get(cursor);
        match key {
            KeyCode::Esc | KeyCode::Char('q') if messages.is_some() => {
                messages = None;
                cursor = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Down | KeyCode::Char('j') => {
                cursor = if messages.is_some() {
                    cursor.saturating_add(1)
                } else {
                    cursor.saturating_add(1).min(sessions.len().saturating_sub(1))
                }
            }
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Enter if messages.is_some() => {
                let chat = room.as_deref().unwrap_or(&bot);
                if let Some(id) = messages
                    .as_ref()
                    .and_then(|value| value["messages"].as_array())
                    .and_then(|rows| rows.last())
                    .and_then(|row| row["id"].as_str())
                    && self.selected.as_deref() == Some(chat)
                    && self.lane(chat).iter().any(|entry| entry.id == id)
                {
                    // 已载入的历史可回到原聊天，其他历史留在预览中。
                    self.overlays.clear();
                    self.pick = Some(id.into());
                    return None;
                }
            }
            KeyCode::Enter if messages.is_none() => {
                if let Some(item) = current.filter(|row| row["hasMessages"] == true) {
                    self.sheet(
                        "sessionHistoryMessages",
                        json!({"botId":bot,"roomId":room,"historyId":item["id"]}),
                        Reply::SessionHistoryMessages { bot: bot.clone(), room: room.clone() },
                    );
                }
            }
            KeyCode::Char('D') if messages.is_none() => {
                if let Some(id) = current.filter(|row| row["canDelete"] == true).and_then(|row| row["id"].as_str()) {
                    let act = ConfirmAct::DeleteOldSession { bot: bot.clone(), room: room.clone(), id: id.into() };
                    // overlay_key 会把返回的历史页放在新打开的确认框下方。
                    self.overlays.push(Overlay::Confirm(Confirm {
                        title: super::super::l10n::tr("Delete old agent session").into_owned(),
                        detail: "The original chat history will be kept.".into(),
                        note: String::new(),
                        button: "Delete",
                        act,
                    }));
                }
            }
            _ => {}
        }
        Some(Overlay::SessionHistory { bot, room, snapshot, messages, cursor })
    }

    /// 活动页在已有聊天中定位结果，或向 Host 读取未加载的结果。
    pub(in crate::tui) fn member_activity_key(
        &mut self,
        bot: String,
        room: String,
        snapshot: Value,
        mut result: Option<Value>,
        mut cursor: usize,
        key: KeyCode,
    ) -> Option<Overlay> {
        match key {
            KeyCode::Esc | KeyCode::Char('q') if result.is_some() => {
                result = None;
                cursor = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Down | KeyCode::Char('j') => cursor = cursor.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Char('o') if result.is_none() => {
                if let Some(id) = snapshot["entry"]["id"].as_str()
                    && self.lane(&room).iter().any(|entry| entry.id == id)
                {
                    // 跳回群聊时展开原卡，并把键盘焦点交给该条目。
                    self.overlays.clear();
                    self.pick = Some(id.into());
                    self.unfolded.insert(id.into());
                    return None;
                }
            }
            KeyCode::Char('e') if result.is_some() => {
                // 结果未载入聊天时也可以保存同一投递的证据。
                if let Some(evidence) = result.as_ref().map(|value| &value["evidence"])
                    && let Some(id) = evidence["id"].as_str()
                    && let Some(files) = evidence["data"]["files"].as_array()
                {
                    for value in files {
                        if let Ok(file) = serde_json::from_value(value.clone()) {
                            self.download_shared(id.to_owned(), file, super::SharedFileAction::Save);
                        }
                    }
                }
            }
            KeyCode::Enter if result.is_none() => {
                let row = self.member_activity_row(&room, &snapshot);
                if let Some(id) = row.and_then(|row| row["resultId"].as_str()).map(str::to_owned) {
                    if self.lane(&room).iter().any(|entry| entry.id == id) {
                        self.overlays.clear();
                        self.pick = Some(id);
                        return None;
                    }
                    self.sheet(
                        "groupDeliveryResult",
                        json!({"roomId":room,"resultId":id}),
                        Reply::MemberDeliveryResult { bot: bot.clone(), room: room.clone() },
                    );
                }
            }
            KeyCode::Char('H') if self.session_history => {
                let context = self
                    .group_contexts
                    .as_ref()
                    .filter(|(id, _)| *id == room)
                    .and_then(|(_, value)| value["members"].as_array())
                    .and_then(|members| members.iter().find(|member| member["botId"] == bot));
                if context.is_some_and(|member| member["shared"] != true) {
                    let scope = Some(room.clone());
                    self.overlays.push(Overlay::MemberActivity { bot: bot.clone(), room, snapshot, result, cursor });
                    self.open_session_history(bot, scope);
                    return None;
                }
            }
            _ => {}
        }
        Some(Overlay::MemberActivity { bot, room, snapshot, result, cursor })
    }

    /// 聊天已更新的卡片优先于弹层最初取得的历史副本。
    pub(in crate::tui) fn member_activity_row<'a>(&'a self, room: &str, snapshot: &'a Value) -> Option<&'a Value> {
        let original =
            snapshot["entry"]["id"].as_str().and_then(|id| self.lane(room).into_iter().find(|entry| entry.id == id));
        let data = original.map_or(&snapshot["entry"]["data"], |entry| &entry.data);
        data["deliveryRows"].as_array()?.iter().find(|row| row["deliveryId"] == snapshot["deliveryId"])
    }
}
