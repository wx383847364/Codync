//! The user's messages waiting behind a running turn: `S` sends the next one now (`steer`), `D` takes back the last
//! one (`unqueue`).

use super::{After, App, Entry, Overlay, model::Status};
use crossterm::event::KeyCode;
use serde_json::{Value, json};

/// The first host that sends a queued message now and takes one back.
const STEER_SINCE: &str = "2.39.0";

impl App {
    /// `bot`'s queued messages, oldest (the next to go) first.
    pub fn queued(&self, bot: &str) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.lane(bot).into_iter().filter(|e| e.queued()).collect();
        v.sort_by_key(|e| e.seq);
        v
    }

    /// `bot`'s chat: its lane without the queued messages, which wait above the composer.
    pub fn chat(&self, bot: &str) -> Vec<&Entry> {
        self.lane(bot).into_iter().filter(|e| !e.queued()).collect()
    }

    /// Whether the host takes `S` and `D`: an older one only queues.
    pub fn queue_keys(&self) -> bool {
        crate::compat::at_least(&self.host_version, STEER_SINCE)
    }

    /// 按卡片顺序列出本群可引导成员及其界面上显示的回合身份。
    pub(in crate::tui) fn group_guidance<'a>(
        &'a self,
        room: &'a str,
        entry: &'a Entry,
    ) -> impl Iterator<Item = (&'a str, &'a str, &'a str)> + 'a {
        entry.data["deliveryRows"].as_array().into_iter().flatten().filter_map(move |row| {
            // 只接受正在同群执行且适配器支持引导的成员。
            let bot = self.bots.get(row["targetBotId"].as_str()?)?;
            (self.online
                && row["canGuide"] == true
                && row["status"] == "queued"
                && bot.status == Status::Working
                && bot.storage_error.is_none()
                && bot.working_chat.as_deref() == Some(room))
            .then_some((row["deliveryId"].as_str()?, bot.guiding_delivery_id.as_deref()?, bot.name.as_str()))
        })
    }

    /// S 引导选中的群投递或最近可引导的群投递；私聊沿用发送下一条排队消息。
    pub(super) fn steer_next(&mut self) {
        if let Some(group) = self.bot().filter(|bot| bot.group) {
            let targets = self
                .lane(&group.id)
                .into_iter()
                .rev()
                .filter(|entry| self.pick.as_ref().is_none_or(|picked| *picked == entry.id))
                .map(|entry| {
                    // 保留打开选择器时的回合 ID，回合变更后由 host 拒绝旧操作。
                    self.group_guidance(&group.id, entry)
                        .map(|(id, active_id, name)| {
                            (json!({"deliveryId": id, "activeDeliveryId": active_id}), name.to_owned())
                        })
                        .collect::<Vec<_>>()
                })
                .find(|targets| !targets.is_empty())
                .unwrap_or_default();
            match targets.as_slice() {
                [] => self.flash("没有可引导的群内派工"),
                [(body, name)] => self.guide_group_member(body.clone(), name),
                _ => self.overlays.push(Overlay::GroupGuidance { targets, cursor: 0 }),
            }
            return;
        }
        let next = self.selected.as_deref().and_then(|bot| self.queued(bot).first().map(|e| e.id.clone()));
        self.on_queued("steer", next, "Sending now…");
    }

    /// 选择一个成员后只提交这一项群投递。
    fn guide_group_member(&mut self, body: Value, name: &str) {
        self.call("guideDelivery", body, After::Nothing);
        self.flash(&format!("引导 {name}…"));
    }

    /// 多目标用方向键选择成员；回车提交当前项，Esc 放弃操作。
    pub(super) fn group_guidance_key(
        &mut self,
        targets: Vec<(Value, String)>,
        mut cursor: usize,
        key: KeyCode,
    ) -> Option<Overlay> {
        match key {
            KeyCode::Esc => return None,
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(targets.len().saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Enter => {
                if let Some((body, name)) = targets.get(cursor) {
                    self.guide_group_member(body.clone(), name);
                }
                return None;
            }
            _ => {}
        }
        Some(Overlay::GroupGuidance { targets, cursor })
    }

    /// `D`: the last queued message never goes out.
    pub(super) fn unqueue_last(&mut self) {
        let last = self.selected.as_deref().and_then(|bot| self.queued(bot).last().map(|e| e.id.clone()));
        self.on_queued("unqueue", last, "Removed");
    }

    /// `method` on the selected bot's queued message `id`, saying `done`.
    fn on_queued(&mut self, method: &'static str, id: Option<String>, done: &str) {
        let Some(bot) = self.selected.clone() else { return };
        let Some(id) = id else {
            self.flash("No message waiting");
            return;
        };
        if self
            .host_supports(STEER_SINCE, "Update Codync on {host}: sending now and taking back need {version} or newer.")
        {
            self.call(method, json!({"botId": bot, "entryId": id}), After::Nothing);
            self.flash(done);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Msg, test_app};
    use serde_json::json;

    fn entry(app: &mut super::App, id: &str, seq: i64, data: &serde_json::Value) {
        let e = json!({"id": id, "botId": "a", "seq": seq, "kind": "user", "turn": seq, "data": data});
        app.on_msg(Msg::Event(json!({"type": "entry", "entry": e})));
    }

    #[test]
    fn queued_lists_waiting_messages_oldest_first_and_never_a_removed_one() {
        let mut app = test_app();
        app.on_msg(Msg::Event(json!({"type": "bot", "bot": {"id": "a", "name": "A"}})));
        entry(&mut app, "sent", 1, &json!({"text": "hi", "status": "sent"}));
        entry(&mut app, "second", 3, &json!({"text": "two", "status": "queued"}));
        entry(&mut app, "first", 2, &json!({"text": "one", "status": "queued"}));
        entry(&mut app, "gone", 4, &json!({"text": "x", "status": "cancelled", "removed": true}));
        let ids: Vec<&str> = app.queued("a").iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["first", "second"]);
        let chat: Vec<&str> = app.chat("a").iter().map(|e| e.id.as_str()).collect();
        assert_eq!(chat, ["sent"], "the chat has neither the waiting messages nor a removed one");
    }

    /// 多人卡保留各自回合身份，用户可以单独选择第二位成员。
    #[tokio::test]
    async fn s_lets_the_user_choose_a_group_member() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.online = true;
        app.selected = Some("room".into());
        for bot in [
            json!({"id":"room","name":"Room","kind":"group","members":["a","b"]}),
            json!({"id":"a","name":"First","status":"working","workingChat":"room","guidingDeliveryId":"running-a"}),
            json!({"id":"b","name":"Second","status":"working","workingChat":"room","guidingDeliveryId":"running-b"}),
        ] {
            app.on_msg(Msg::Event(json!({"type":"bot","bot":bot})));
        }
        app.on_msg(Msg::Event(
            json!({"type":"entry","entry":{"id":"card","botId":"room","seq":1,"kind":"notice","data":{
                "deliveryRows":[
                    {"deliveryId":"guide-a","targetBotId":"a","status":"queued","canGuide":true},
                    {"deliveryId":"guide-b","targetBotId":"b","status":"queued","canGuide":true}
                ]
            }}}),
        ));
        app.pick = Some("card".into());
        app.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert!(matches!(app.overlays.last(), Some(super::Overlay::GroupGuidance { cursor: 0, targets })
            if targets[1].0["activeDeliveryId"] == "running-b"));
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.hint.take().map(|(text, _)| text).as_deref(), Some("引导 Second…"));
    }

    #[tokio::test]
    async fn s_and_d_act_on_a_waiting_message_on_a_host_that_takes_them() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.on_msg(Msg::Event(json!({"type": "bot", "bot": {"id": "a", "name": "A"}})));
        app.selected = Some("a".into());
        app.host = "test".into();
        let hint = |app: &mut super::App, c: char| {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT));
            app.hint.take().map(|(text, _)| text).unwrap_or_default()
        };
        assert_eq!(hint(&mut app, 'S'), "No message waiting");
        entry(&mut app, "q", 1, &json!({"text": "one", "status": "queued"}));
        app.host_version = "2.38.0".into();
        assert!(!app.queue_keys());
        assert_eq!(hint(&mut app, 'S'), "Update Codync on test: sending now and taking back need 2.39.0 or newer.");
        app.host_version = "2.39.0".into();
        assert!(app.queue_keys());
        assert_eq!(hint(&mut app, 'S'), "Sending now…");
        assert_eq!(hint(&mut app, 'D'), "Removed");
    }
}
