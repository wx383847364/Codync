//! The new/edit group chat form.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

use super::{After, App, Bot, Editor, Focus, GroupField, GroupForm, Overlay};

impl App {
    /// 当前聊天选中群时返回它的 ID，供菜单显示和发起创建使用。
    pub(super) fn selected_group_id(&self) -> Option<&str> {
        self.selected.as_deref().filter(|id| self.group_sessions && self.bots.get(*id).is_some_and(|bot| bot.group))
    }

    /// 新版 Host 先确认预填成员，旧版 Host 仍能直接复制。
    pub(super) fn create_group_session(&mut self) {
        let Some(id) = self.selected_group_id().map(str::to_owned) else { return };
        if !self.group_member_picker {
            self.call("createGroupSession", json!({"botId":id}), After::GroupSessionCreated);
            return;
        }
        let Some(source) = self.bots.get(&id) else { return };
        let mut form = self.group_create_form();
        form.session_source = Some(id);
        let title = super::super::l10n::tr("{name} · Auto-numbered").replace("{name}", &source.name);
        form.name = Editor::with(&title);
        form.members.clone_from(&source.members);
        form.leader = source.group_leader().map(str::to_owned);
        self.overlays.push(Overlay::Group(form));
    }

    /// 保存响应先更新名单，创建操作随后将焦点交给新聊天的输入框。
    pub(super) fn receive_saved_bot(&mut self, bot: &Value, created: bool) {
        if let Some(id) = bot["id"].as_str() {
            self.upsert_bot(bot);
            if created {
                self.select(id);
                self.chat_page = true;
                self.focus = Focus::Chat;
                self.typing = true;
            }
        }
    }

    /// 打开待选成员的普通建群表单。
    pub(super) fn new_group(&mut self) {
        self.overlays.push(Overlay::Group(self.group_create_form()));
    }

    /// 群创建和新会话共用搜索字段与默认接收人选择。
    fn group_create_form(&self) -> GroupForm {
        GroupForm {
            group_id: None,
            session_source: None,
            query: Editor::default(),
            leader: None,
            show_leader: self.group_member_picker,
            name: Editor::default(),
            about: Editor::default(),
            members: vec![],
            field: GroupField::Search,
            cursor: 0,
            error: None,
            saving: false,
        }
    }

    /// 搜索同一台电脑的机器人，也保留已加入的隐藏成员。
    pub fn group_candidates(&self, form: &GroupForm) -> Vec<&Bot> {
        let query = form.query.text.trim().to_lowercase();
        let inherited = form.session_source.as_ref().and_then(|id| self.bots.get(id));
        self.roster()
            .into_iter()
            .filter(|bot| !bot.group)
            .chain(self.bots.values().filter(|bot| {
                bot.hidden
                    && !bot.group
                    && (form.members.contains(&bot.id)
                        || inherited.is_some_and(|source| source.members.contains(&bot.id)))
            }))
            .filter(|bot| query.is_empty() || bot.name.to_lowercase().contains(&query))
            .collect()
    }

    /// "Alice, Bob" when no name is typed.
    pub fn group_default_name(&self, members: &[String]) -> String {
        let names: Vec<&str> = members.iter().filter_map(|m| self.bots.get(m)).map(|b| b.name.as_str()).collect();
        names.join(", ")
    }

    /// 搜索后用上下键浏览、空格加入，负责人字段用左右键切换已加入成员。
    pub(super) fn group_key(&mut self, mut g: GroupForm, k: KeyEvent) -> Option<Overlay> {
        // 保存回执由表单接收，提交中保持该表单在栈顶。
        if g.saving {
            return Some(Overlay::Group(g));
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let candidates = self.group_candidates(&g);
        let n = candidates.len();
        g.cursor = g.cursor.min(n.saturating_sub(1));
        let about = g.group_id.is_some();
        match k.code {
            KeyCode::Esc => return None,
            KeyCode::Enter if g.field == GroupField::Search => g.field = GroupField::Bots,
            KeyCode::Enter => self.save_group(&mut g),
            KeyCode::Char('s') if ctrl => self.save_group(&mut g),
            KeyCode::Tab => g.field = g.field.step(about, g.show_leader, false),
            KeyCode::BackTab => g.field = g.field.step(about, g.show_leader, true),
            KeyCode::Down if g.field != GroupField::Bots => g.field = g.field.step(about, g.show_leader, false),
            KeyCode::Down => g.cursor = (g.cursor + 1).min(n.saturating_sub(1)),
            KeyCode::Up if g.field != GroupField::Bots || g.cursor == 0 => {
                g.field = g.field.step(about, g.show_leader, true);
            }
            KeyCode::Up => g.cursor -= 1,
            KeyCode::Left | KeyCode::Right if g.field == GroupField::Leader => {
                let choices = g.members.len() + 1;
                let current = g.members.iter().position(|id| g.leader.as_ref() == Some(id)).map_or(0, |i| i + 1);
                let step = if k.code == KeyCode::Left { choices - 1 } else { 1 };
                g.leader = g.members.get(((current + step) % choices).wrapping_sub(1)).cloned();
            }
            KeyCode::Char(' ') if g.field == GroupField::Bots => {
                if let Some(id) = candidates.get(g.cursor).map(|bot| bot.id.clone()) {
                    g.error = None;
                    if let Some(i) = g.members.iter().position(|member| *member == id) {
                        g.members.remove(i);
                        if g.leader.as_ref() == Some(&id) {
                            g.leader = None;
                        }
                    } else {
                        g.members.push(id);
                    }
                }
            }
            _ if g.field == GroupField::Name && g.session_source.is_none() => {
                g.name.key(k);
            }
            _ if g.field == GroupField::About => {
                g.about.key(k);
            }
            _ if g.field == GroupField::Search => {
                g.query.key(k);
                g.cursor = 0;
            }
            _ => {}
        }
        Some(Overlay::Group(g))
    }

    /// 提交建群或新会话，创建成功后选中新的聊天。
    fn save_group(&mut self, g: &mut GroupForm) {
        if g.saving {
            return;
        }
        if g.members.is_empty() {
            g.error = Some("Pick at least one bot.".into());
            return;
        }
        let typed = g.name.text.trim();
        let name = if typed.is_empty() { self.group_default_name(&g.members) } else { typed.to_owned() };
        let mut body = json!({"name": name, "members": g.members, "description": g.about.text.trim()});
        g.saving = true;
        g.error = None;
        if let Some(source) = &g.session_source {
            self.call(
                "createGroupSession",
                json!({"botId":source,"selection":{
                    "members":g.members,"groupLeaderId":g.leader
                }}),
                After::Saved,
            );
        } else if let Some(id) = &g.group_id {
            body["id"] = id.clone().into();
            self.call("updateBot", body, After::Saved);
        } else {
            body["id"] = "".into();
            body["kind"] = "group".into();
            if g.show_leader {
                body["groupLeaderId"] = json!(g.leader);
            }
            self.call("createBot", body, After::Saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_app;
    use super::*;

    #[test]
    fn a_new_group_has_no_about_and_an_edited_one_keeps_it() {
        let mut app = test_app();
        app.bots.insert("a".into(), Bot::parse(&json!({"id": "a", "name": "Ada"})).expect("a bot"));
        app.new_group();
        let Some(Overlay::Group(mut g)) = app.overlays.pop() else { panic!("the group form opens") };
        let mut fields = vec![];
        for code in [KeyCode::Tab, KeyCode::Tab, KeyCode::BackTab, KeyCode::Up, KeyCode::Up] {
            let Some(Overlay::Group(next)) = app.group_key(g, KeyEvent::from(code)) else { panic!("stays open") };
            g = next;
            fields.push(g.field);
        }
        let (n, b) = (GroupField::Name, GroupField::Bots);
        assert_eq!(fields, [b, n, b, GroupField::Search, n], "creating skips About");
        let Some(Overlay::Group(g)) = app.group_key(g, KeyEvent::from(KeyCode::Char('x'))) else { panic!("stays") };
        assert_eq!(g.name.text, "x");
        let group = json!({"id": "g", "kind": "group", "name": "Team", "members": ["a"]});
        app.bots.insert("g".into(), Bot::parse(&group).expect("a group"));
        app.selected = Some("g".into());
        app.edit_bot();
        let Some(Overlay::Group(mut g)) = app.overlays.pop() else { panic!("the group editor opens") };
        let mut fields = vec![];
        for _ in 0..3 {
            let Some(Overlay::Group(next)) = app.group_key(g, KeyEvent::from(KeyCode::Tab)) else { panic!("stays") };
            g = next;
            fields.push(g.field);
        }
        assert_eq!(fields, [GroupField::About, b, n]);
    }
}
