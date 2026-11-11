//! The composer's `/` commands, listed while the draft is just `/partial`: Fast mode runs here
//! (`updateBot`); a skill fills in `/<id> `, which the host turns into "read this skill first"
//! (`market::expand_command`). A group offers the same, over its members.

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::chat::workspace_files::{Candidate, Reference};

use super::{After, App, Bot, Editor};

/// What picking a command does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashAction {
    Fast,
    DevServers,
    Skill,
    Mention,
    Session,
    File(Reference),
}

pub(super) struct WorkspaceSearch {
    id: Uuid,
    bot: String,
    query: String,
    result: Option<Result<Vec<Candidate>, String>>,
}

/// A command the list offers.
pub struct SlashCommand {
    /// What follows the `/`.
    pub name: String,
    pub title: String,
    pub detail: String,
    pub action: SlashAction,
}

impl SlashCommand {
    /// The row's text after `/name`.
    /// 描述命令的用户行为，包含群成员选择和会话切换选项。
    pub fn about(&self) -> String {
        match self.action {
            SlashAction::Mention | SlashAction::File(_) => self.detail.clone(),
            SlashAction::Skill if self.detail.is_empty() => self.title.clone(),
            SlashAction::Skill => self.detail.replace('\n', " "),
            SlashAction::Fast | SlashAction::DevServers | SlashAction::Session => {
                format!("{} · {}", self.title, self.detail)
            }
        }
    }
}

impl App {
    /// The bots the commands reach: the selected bot, or a group's members.
    fn reached(&self) -> Vec<&Bot> {
        let Some(bot) = self.bot() else { return vec![] };
        if bot.group { bot.members.iter().filter_map(|m| self.bots.get(m)).collect() } else { vec![bot] }
    }

    /// The bots `/fast` sets: those reached that offer Fast mode.
    fn fast_bots(&self) -> Vec<&Bot> {
        self.reached().into_iter().filter(|b| b.fast_shown()).collect()
    }

    /// Fast mode, when the bot (or any member of a group) offers it.
    fn built_in(&self) -> Vec<SlashCommand> {
        let bots = self.fast_bots();
        let detail = match bots.as_slice() {
            [] => None,
            [bot] if self.bot().is_some_and(|b| !b.group) => Some(bot.fast_detail()),
            _ => Some(group_fast_detail(&bots)),
        };
        let fast = detail.map(|detail| SlashCommand {
            name: "fast".into(),
            title: "Fast mode".into(),
            detail,
            action: SlashAction::Fast,
        });
        let servers = self.bot().filter(|bot| !bot.dev_servers.is_empty()).map(|bot| SlashCommand {
            name: "servers".into(),
            title: "Dev servers".into(),
            detail: bot.dev_servers.iter().map(|server| server.name.as_str()).collect::<Vec<_>>().join(", "),
            action: SlashAction::DevServers,
        });
        fast.into_iter().chain(servers).collect()
    }

    /// Built-in commands, then the skills the bot (or any member of a group) has on, whose name
    /// or title holds the `/partial` being typed.
    /// 按来源列出适用命令，群成员选择与会话操作保持四端一致。
    pub fn slash_commands(&self) -> Vec<SlashCommand> {
        let draft = self.draft();
        if self.file_query().is_none()
            && let Some((start, end)) = self.mention_query()
        {
            let query = draft.text[start + 1..end].to_lowercase();
            return self
                .reached()
                .into_iter()
                .filter(|bot| bot.name.to_lowercase().contains(&query))
                .map(|bot| SlashCommand {
                    name: bot.id.clone(),
                    title: bot.name.clone(),
                    detail: bot.id.chars().take(8).collect(),
                    action: SlashAction::Mention,
                })
                .collect();
        }
        let mut commands = self.existing_slash_commands(&draft.text);
        if let Some(Ok(candidates)) = self.current_workspace_search().and_then(|search| search.result.as_ref()) {
            // 追加文件结果，不改变已有命令的默认位置。
            commands.extend(candidates.iter().map(|candidate| SlashCommand {
                name: candidate.name.clone(),
                title: candidate.name.clone(),
                detail: if candidate.source.is_empty() {
                    candidate.file.path.clone()
                } else {
                    format!("{} · {}", candidate.source, candidate.file.path)
                },
                action: SlashAction::File(candidate.file.clone()),
            }));
        }
        commands
    }

    /// 保留原有命令和技能的匹配规则，文件结果独立于该规则。
    fn existing_slash_commands(&self, text: &str) -> Vec<SlashCommand> {
        let Some((query, None)) = crate::market::slash_command(text) else { return vec![] };
        if query.starts_with("new") && self.bot().is_some_and(|b| !b.group) {
            return [
                ("new", "after", "在当前任务结束后新建会话"),
                ("new-now", "immediate", "立即停止并新建"),
                ("new-cancel", "cancel", "取消切换"),
            ]
            .into_iter()
            .filter(|(name, _, _)| name.contains(query))
            .map(|(name, mode, title)| SlashCommand {
                name: name.into(),
                title: title.into(),
                detail: mode.into(),
                action: SlashAction::Session,
            })
            .collect();
        }
        let query = query.to_lowercase();
        let bots = self.reached();
        let skills = self.plugins.1.iter().filter_map(|s| {
            let id = s["id"].as_str()?;
            bots.iter().any(|b| b.skills.iter().any(|on| on == id)).then(|| SlashCommand {
                name: id.to_owned(),
                title: s["name"].as_str().unwrap_or(id).to_owned(),
                detail: s["description"].as_str().unwrap_or_default().to_owned(),
                action: SlashAction::Skill,
            })
        });
        self.built_in()
            .into_iter()
            .chain(skills)
            .filter(|c| c.name.contains(&query) || c.title.to_lowercase().contains(&query))
            .collect()
    }

    /// The highlighted row of `count`: kept while the draft stays the same, else the first.
    pub fn slash_selected(&self, count: usize) -> usize {
        let (draft, index) = &self.slash_pick;
        if *draft == self.draft().text { (*index).min(count.saturating_sub(1)) } else { 0 }
    }

    /// Keys while the list is up: ↑/↓ move, Tab or Return runs the highlighted command (Return
    /// sends a skill whose name is already typed out). Returns whether it took the key.
    /// 以键盘操作候选列表，插入成员后保留光标前后正文。
    pub(super) fn slash_key(&mut self, k: KeyEvent) -> bool {
        if !k.modifiers.is_empty() {
            return false;
        }
        let commands = self.slash_commands();
        let typed = self.draft().text;
        if k.code == KeyCode::Esc && self.file_query().is_some() {
            self.mention_dismissed = Some((typed, self.draft().cursor));
            return true;
        }
        let Some(n) = std::num::NonZeroUsize::new(commands.len()).map(usize::from) else {
            // 等待文件名匹配时保护回车，正文中的多段路径仍可直接发送。
            return k.code == KeyCode::Enter
                && self.file_query().is_some_and(|(start, end)| start == 0 || !typed[start + 1..end].contains('/'))
                && self.current_workspace_search().is_none_or(|search| search.result.is_none());
        };
        let sel = self.slash_selected(n);
        match k.code {
            KeyCode::Down => self.slash_pick = (typed, (sel + 1) % n),
            KeyCode::Up => self.slash_pick = (typed, (sel + n - 1) % n),
            KeyCode::Esc if commands[sel].action == SlashAction::Mention => {
                self.mention_dismissed = Some((typed, self.draft().cursor));
            }
            KeyCode::Tab | KeyCode::Enter => {
                if k.kind == crossterm::event::KeyEventKind::Repeat {
                    return true;
                }
                let picked = &commands[sel];
                if k.code == KeyCode::Enter && picked.action == SlashAction::Skill && typed[1..] == picked.name {
                    return false;
                }
                self.run_slash(picked);
            }
            _ => return false,
        }
        true
    }

    /// A draft that is just a built-in command (`/fast`) runs it instead of being sent.
    /// Returns whether it did.
    pub(super) fn built_in_command(&mut self, text: &str) -> bool {
        let Some(name) = text.strip_prefix('/') else { return false };
        let Some(command) = self.built_in().into_iter().find(|c| c.name == name) else { return false };
        self.run_slash(&command);
        true
    }

    /// 光标处的查询；Esc 只关闭当前片段，文本或光标变化后重新出现。
    fn mention_query(&self) -> Option<(usize, usize)> {
        if !self.bot()?.group {
            return None;
        }
        let draft = self.draft();
        if self.mention_dismissed.as_ref() == Some(&(draft.text.clone(), draft.cursor)) {
            return None;
        }
        let end = draft.cursor;
        let at = draft.text[..end].rfind('@')?;
        if at > 0 && !draft.text[..at].chars().next_back()?.is_whitespace() {
            return None;
        }
        let offset = draft.text[..at].encode_utf16().count();
        if draft
            .mentions
            .iter()
            .any(|mention| mention.start <= offset && offset < mention.end && mention.workspace_file.is_some())
        {
            return None;
        }
        let query = &draft.text[at + 1..end];
        (!query.contains('\n') && query.encode_utf16().count() <= 24).then_some((at, end))
    }

    /// 执行所选命令，群操作逐成员发送可信 host 意图。
    fn run_slash(&mut self, command: &SlashCommand) {
        if command.action == SlashAction::Session {
            if let Some(bot) = &self.selected {
                self.call("newSession", json!({"botId":bot,"mode":command.detail}), After::Nothing);
            }
            if let Some(bot) = &self.selected {
                self.drafts.entry(bot.clone()).or_default().clear();
            }
            return;
        }
        if matches!(command.action, SlashAction::Mention | SlashAction::File(_)) {
            let (range, member, file) = if let SlashAction::File(file) = &command.action {
                (self.file_query(), None, Some(file.clone()))
            } else {
                (self.mention_query(), Some(command.name.clone()), None)
            };
            if let Some((start, end)) = range
                && let Some(key) = self.selected.clone()
            {
                let draft = self.drafts.entry(key).or_default();
                let before = draft.text[..start].encode_utf16().count();
                let token = format!("@{}", command.title);
                draft.replace_range(start, end, &format!("{token} "));
                draft.mentions.push(crate::chat::mentions::Mention {
                    bot_id: member,
                    start: before,
                    end: before + token.encode_utf16().count(),
                    workspace_file: file,
                });
            }
            return;
        }
        let fill = match command.action {
            SlashAction::Session | SlashAction::Mention | SlashAction::File(_) => unreachable!("handled above"),
            // Each bot's own setting, so it holds in its own chat too.
            SlashAction::Fast => {
                let bots = self.fast_bots();
                let on = !bots.is_empty() && bots.iter().all(|b| b.fast_mode);
                for b in bots {
                    self.call("updateBot", json!({"id": b.id, "fastMode": !on}), After::Nothing);
                }
                self.flash(if on { "Fast mode off" } else { "Fast mode on" });
                String::new()
            }
            SlashAction::DevServers => {
                self.open_dev_servers();
                String::new()
            }
            SlashAction::Skill => format!("/{} ", command.name),
        };
        if let Some(key) = self.selected.clone() {
            self.drafts.insert(key, Editor::with(&fill));
        }
    }

    /// 光标前的斜杠词可以包含相对路径，只有空白或正文开头的斜杠触发。
    fn file_query(&self) -> Option<(usize, usize)> {
        if !self.online || !self.workspace_file_search {
            return None;
        }
        let draft = self.drafts.get(self.selected.as_ref()?)?;
        if self.mention_dismissed.as_ref().is_some_and(|(text, caret)| text == &draft.text && *caret == draft.cursor) {
            return None;
        }
        if draft.text[draft.cursor..].starts_with(|ch: char| !ch.is_whitespace()) {
            return None;
        }
        let prefix = &draft.text[..draft.cursor];
        let token = prefix.rsplit(char::is_whitespace).next()?;
        (token.starts_with('/') && !token.starts_with("//") && token.encode_utf16().count() <= 256)
            .then_some((draft.cursor - token.len(), draft.cursor))
    }

    /// 仅展示当前聊天和光标查询的结果。
    fn current_workspace_search(&self) -> Option<&WorkspaceSearch> {
        let (start, end) = self.file_query()?;
        let draft = self.drafts.get(self.selected.as_ref()?)?;
        let state = self.workspace_search.as_ref()?;
        (self.selected.as_deref() == Some(&state.bot) && draft.text[start + 1..end] == state.query).then_some(state)
    }

    /// 返回等待或无结果状态，终端通过现有列表位置显示。
    pub fn workspace_search_status(&self) -> Option<&'static str> {
        self.file_query()?;
        match self.current_workspace_search().and_then(|search| search.result.as_ref()) {
            None => Some("Searching workspace files…"),
            Some(Err(_)) => Some("Could not search workspace files."),
            Some(Ok(items)) if items.is_empty() => Some("No matching workspace files"),
            Some(Ok(_)) => None,
        }
    }

    /// 只应用当前请求的结果，避免较早的响应覆盖新结果。
    pub(super) fn workspace_files_answered(&mut self, id: Uuid, reply: Result<Value, String>) {
        if let Some(search) = self.workspace_search.as_mut().filter(|search| search.id == id) {
            search.result =
                Some(reply.and_then(|value| {
                    serde_json::from_value(value["items"].clone()).map_err(|error| error.to_string())
                }));
        }
    }

    /// 开始斜杠查询时刷新技能；向同一个已连接的 Host 请求工作区候选。
    pub(super) fn slash_typed(&mut self) {
        let Some(draft) = self.selected.as_ref().and_then(|bot| self.drafts.get(bot)) else {
            self.workspace_search = None;
            return;
        };
        if draft.text == "/" {
            self.call("skills", json!({}), After::Skills);
        }
        let Some((start, end)) = self.file_query() else {
            self.workspace_search = None;
            return;
        };
        if self.current_workspace_search().is_some() {
            return;
        }
        if let Some(bot) = self.selected.clone() {
            let query = draft.text[start + 1..end].to_owned();
            let id = Uuid::new_v4();
            self.call("searchWorkspaceFiles", json!({"botId": bot, "query": query}), After::WorkspaceFiles(id));
            self.workspace_search = Some(WorkspaceSearch { id, bot, query, result: None });
        }
    }
}

/// A group's Fast mode: on when every member offering it has it on; then the first member whose
/// agent refused it says why.
fn group_fast_detail(bots: &[&Bot]) -> String {
    if !bots.iter().all(|b| b.fast_mode) {
        return "Turn on for every bot here: faster replies and more usage".into();
    }
    bots.iter()
        .find_map(|b| b.fast_refusal().map(|why| format!("{}: {why}", b.name)))
        .unwrap_or_else(|| "Turn off for every bot here and return to standard speed".into())
}

#[cfg(test)]
mod tests {
    use super::super::{Bot, test_app};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    fn app_with(bot: &serde_json::Value) -> super::App {
        let mut app = test_app();
        app.bots.insert("b".into(), Bot::parse(bot).expect("a bot parses"));
        app.selected = Some("b".into());
        app.plugins.1 = vec![
            json!({"id": "pdf", "name": "pdf", "description": "PDFs"}),
            json!({"id": "docx", "name": "docx"}),
            json!({"id": "xlsx", "name": "xlsx"}),
        ];
        app
    }

    fn names(app: &super::App) -> Vec<String> {
        app.slash_commands().into_iter().map(|c| c.name).collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// 选中文件后长按回车的重复事件不能把草稿送出。
    #[tokio::test]
    async fn file_confirmation_and_enter_repeat_keep_the_draft() {
        let mut app = app_with(&json!({"id":"b","name":"Bot","fastModeSupport":"unsupported"}));
        app.online = true;
        app.typing = true;
        app.workspace_file_search = true;
        app.drafts.insert("b".into(), super::Editor::with("Read /guide"));
        app.workspace_search = Some(super::WorkspaceSearch {
            id: super::Uuid::new_v4(),
            bot: "b".into(),
            query: "guide".into(),
            result: Some(Ok(vec![super::Candidate {
                name: "guide.md".into(),
                source: String::new(),
                file: super::Reference { path: "docs/guide.md".into(), workspace: "root".into() },
            }])),
        });

        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.draft().text, "Read @guide.md ");
        assert_eq!(app.draft().mentions.len(), 1);
        app.on_key(KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, crossterm::event::KeyEventKind::Repeat));
        assert!(app.sends.is_empty());
    }

    #[tokio::test]
    /// 只在对应前缀列出新增会话选项，原内置和技能排序保持一致。
    async fn the_list_offers_built_in_commands_then_the_bots_skills() {
        let mut app =
            app_with(&json!({"id": "b", "name": "Bot", "skills": ["pdf", "docx"], "fastModeSupport": "offered"}));
        app.drafts.insert("b".into(), super::Editor::with("/"));
        assert_eq!(names(&app), ["fast", "pdf", "docx"], "only skills the bot has on");
        app.drafts.insert("b".into(), super::Editor::with("/new"));
        assert_eq!(names(&app), ["new", "new-now", "new-cancel"], "new session defaults to waiting");
        app.bots.get_mut("b").expect("the bot").fast_support = "unknown".into();
        app.drafts.insert("b".into(), super::Editor::with("/"));
        assert_eq!(names(&app), ["fast", "pdf", "docx"], "offered until the agent says otherwise");
        app.bots.get_mut("b").expect("the bot").fast_support = "unsupported".into();
        assert_eq!(names(&app), ["pdf", "docx"], "no Fast mode on offer");
    }

    #[tokio::test]
    async fn arrows_pick_a_row_and_return_runs_it() {
        let mut app = app_with(&json!({"id": "b", "name": "Bot", "skills": ["pdf"], "fastModeSupport": "offered"}));
        app.drafts.insert("b".into(), super::Editor::with("/"));
        assert!(app.slash_key(key(KeyCode::Up)), "up wraps to the last row");
        assert_eq!(app.slash_selected(2), 1);
        assert!(app.slash_key(key(KeyCode::Enter)), "Return on a skill fills it in");
        assert_eq!(app.draft().text, "/pdf ");
        assert!(names(&app).is_empty(), "the list closes once the name is done");
        app.drafts.insert("b".into(), super::Editor::with("/pdf"));
        assert!(!app.slash_key(key(KeyCode::Enter)), "a skill already typed out sends");
        app.drafts.insert("b".into(), super::Editor::with("/fa"));
        assert!(app.slash_key(key(KeyCode::Tab)), "a built-in command runs");
        assert_eq!(app.draft().text, "", "and the draft clears");
    }

    #[tokio::test]
    async fn a_group_offers_its_members_fast_mode_and_skills() {
        let mut app = app_with(&json!({"id": "b", "name": "Bea", "skills": ["docx"], "fastModeSupport": "offered"}));
        let members = [("c", "Cal", json!(["xlsx", "docx"])), ("d", "Dot", json!([]))];
        for (id, name, skills) in members {
            let bot = json!({"id": id, "name": name, "skills": skills, "fastModeSupport": "offered"});
            app.bots.insert(id.into(), Bot::parse(&bot).expect("a bot parses"));
        }
        let group = json!({"id": "g", "kind": "group", "members": ["b", "c", "d"]});
        app.bots.insert("g".into(), Bot::parse(&group).expect("a group parses"));
        app.selected = Some("g".into());
        app.drafts.insert("g".into(), super::Editor::with("/"));
        assert_eq!(names(&app), ["fast", "docx", "xlsx"], "every member's skills, once each");
        let detail = |app: &super::App| app.slash_commands().remove(0).detail;
        assert!(detail(&app).starts_with("Turn on for every bot here"));
        app.bots.get_mut("b").expect("Bea").fast_mode = true;
        assert!(detail(&app).starts_with("Turn on"), "on only when every member has it on");
        app.bots.get_mut("d").expect("Dot").fast_support = "unsupported".into();
        app.bots.get_mut("c").expect("Cal").fast_mode = true;
        assert!(detail(&app).starts_with("Turn off for every bot here"), "Dot has no Fast mode to count");
        let cal = app.bots.get_mut("c").expect("Cal");
        (cal.fast_support, cal.fast_issue) = ("refused".into(), Some("requires extra usage".into()));
        assert!(detail(&app).starts_with("Cal: The agent turned it off: requires extra usage"));
        assert!(app.built_in_command("/fast"), "a bare /fast runs in a group too");
        assert_eq!(app.hint.as_ref().map(|(text, _)| text.as_str()), Some("Fast mode off"));
        for id in ["b", "c"] {
            let bot = app.bots.get_mut(id).expect("a member");
            (bot.fast_mode, bot.fast_support) = (false, "unsupported".into());
        }
        app.drafts.insert("g".into(), super::Editor::with("/"));
        assert_eq!(names(&app), ["docx", "xlsx"], "no member offers Fast mode");
        assert!(!app.built_in_command("/fast"), "then it reaches the bots as typed");
    }

    #[tokio::test]
    async fn servers_opens_the_sheet_only_while_the_bot_has_some() {
        let mut app = app_with(&json!({"id": "b", "name": "Bot", "devServers": []}));
        assert!(!names(&app).contains(&"servers".to_owned()));
        assert!(!app.built_in_command("/servers"), "reaches the agent as typed");
        assert!(app.overlays.is_empty());

        let server = json!({"id": "a", "name": "web", "status": "running", "ports": [], "managed": true});
        let mut app = app_with(&json!({"id": "b", "name": "Bot", "devServers": [server]}));
        app.drafts.insert("b".into(), super::Editor::with("/"));
        assert!(names(&app).contains(&"servers".to_owned()));
        assert!(!app.built_in_command("/servers now"), "only the bare command");
        assert!(app.built_in_command("/servers"));
        assert!(matches!(app.overlays.last(), Some(super::super::Overlay::DevServers(_))));
    }

    #[tokio::test]
    async fn sending_just_a_built_in_command_runs_it() {
        let mut app = app_with(&json!({"id": "b", "name": "Bot", "fastModeSupport": "offered"}));
        assert!(!app.built_in_command("/fast please"), "only the bare command");
        assert!(!app.built_in_command("/new"), "reaches the agent as typed");
        assert!(app.built_in_command("/fast"));
        app.bots.get_mut("b").expect("the bot").fast_support = "unsupported".into();
        assert!(!app.built_in_command("/fast"), "without Fast mode it reaches the agent");
    }
}
