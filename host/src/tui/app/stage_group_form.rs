//! 终端阶段群表单；角色选择与首条草稿复用群菜单和编辑器。

use super::{After, App, Bot, Editor, Overlay};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

pub struct StageGroupForm {
    source: String,
    request_id: String,
    pub name: Editor,
    pub goal: Editor,
    pub acceptance: Editor,
    pub roles: Vec<StageRole>,
    pub cursor: usize,
    pub field: usize,
    pub leader: Option<String>,
    pub from_stage: bool,
    pub handoff: Option<crate::chat::files::SharedFile>,
    pub use_handoff: bool,
    pub saving: bool,
    pub error: Option<String>,
}

pub struct StageRole {
    pub id: String,
    pub source_rev: i64,
    pub instructions: Editor,
    pub selected: bool,
    pub project: Option<String>,
    pub reuse_project: bool,
    pub shared_memory: bool,
}

impl StageGroupForm {
    /// 粘贴只写入当前选中的字段，角色名单本身无需文本输入。
    pub(super) fn editor(&mut self) -> Option<&mut Editor> {
        match self.field {
            0 => Some(&mut self.name),
            1 => Some(&mut self.goal),
            2 => Some(&mut self.acceptance),
            4 => self.roles.get_mut(self.cursor).filter(|role| role.selected).map(|role| &mut role.instructions),
            _ => None,
        }
    }
}

/// 详情同一时刻只显示一种材料，使用只读页面时不会修改交接正文。
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum StageDetailsPage {
    #[default]
    Handoff,
    Incoming,
    Usage,
    Evidence,
}

#[derive(Default)]
pub struct StageDetailsForm {
    room: String,
    pub state: Option<Value>,
    pub markdown: Editor,
    pub evidence: Editor,
    base_version: Option<String>,
    pub incoming: Editor,
    pub usage: Editor,
    pub page: StageDetailsPage,
    pub evidence_index: usize,
    pub evidence_focused: bool,
    pub busy: bool,
    pub error: Option<String>,
}

impl StageDetailsForm {
    /// 关闭后的交接仅浏览，草稿输入和保存均不可用。
    pub(in crate::tui) fn closed(&self) -> bool {
        self.state.as_ref().is_some_and(|v| v["bot"]["stage"]["closedAt"].is_number())
    }

    /// 选择当前可见的 Markdown 或证据编辑器，用于输入和移动光标。
    pub(in crate::tui) fn editor(&mut self) -> &mut Editor {
        match self.page {
            StageDetailsPage::Usage => &mut self.usage,
            StageDetailsPage::Incoming => &mut self.incoming,
            StageDetailsPage::Evidence => &mut self.evidence,
            StageDetailsPage::Handoff if self.evidence_focused => &mut self.evidence,
            StageDetailsPage::Handoff => &mut self.markdown,
        }
    }
}

impl App {
    /// 只在支持该功能的 Host 上为来源群开放创建表单。
    pub(in crate::tui) fn stage_source(&self) -> Option<&Bot> {
        self.selected.as_ref().and_then(|id| self.bots.get(id)).filter(|bot| self.stage_groups && bot.group)
    }

    /// 保留来源群的成员顺序，用户选择所需角色和负责人。
    pub(super) fn open_stage_group(&mut self) {
        let Some(source) = self.stage_source() else { return };
        let roles = source
            .members
            .iter()
            .filter_map(|id| self.bots.get(id))
            .map(|bot| StageRole {
                id: bot.id.clone(),
                source_rev: bot.rev,
                instructions: Editor::with(&bot.description),
                selected: false,
                project: (!bot.managed_workspace && !bot.cwd.is_empty()).then(|| bot.cwd.clone()),
                reuse_project: false,
                shared_memory: bot.stage_memory_shared,
            })
            .collect();
        self.overlays.push(Overlay::StageGroup(Box::new(StageGroupForm {
            source: source.id.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            name: Editor::default(),
            goal: Editor::default(),
            acceptance: Editor::default(),
            roles,
            cursor: 0,
            field: 0,
            leader: None,
            from_stage: source.stage.is_some(),
            handoff: source
                .stage
                .as_ref()
                .and_then(|stage| stage.handoff.as_ref())
                .filter(|handoff| handoff.accepted_at.is_some())
                .map(|handoff| handoff.file.clone()),
            use_handoff: false,
            saving: false,
            error: None,
        })));
    }

    /// Tab 切换表单字段，角色列表上的快捷键只修改本次预览。
    pub(super) fn stage_group_key(&mut self, mut form: Box<StageGroupForm>, key: KeyEvent) -> Option<Overlay> {
        if form.saving {
            return Some(Overlay::StageGroup(form));
        }
        match key.code {
            KeyCode::Esc => return None,
            KeyCode::Tab => form.field = (form.field + 1) % 5,
            KeyCode::BackTab => form.field = (form.field + 4) % 5,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => self.save_stage_group(&mut form),
            KeyCode::F(2) if form.handoff.is_some() => form.use_handoff = !form.use_handoff,
            KeyCode::Up if form.field == 3 => form.cursor = form.cursor.saturating_sub(1),
            KeyCode::Down if form.field == 3 => form.cursor = (form.cursor + 1).min(form.roles.len().saturating_sub(1)),
            _ => match form.field {
                0 => {
                    form.name.key(key);
                }
                1 => {
                    form.goal.key(key);
                }
                2 => {
                    form.acceptance.key(key);
                }
                3 => {
                    if let Some(role) = form.roles.get_mut(form.cursor) {
                        match key.code {
                            KeyCode::Char(' ') => {
                                role.selected = !role.selected;
                                if !role.selected && form.leader.as_ref() == Some(&role.id) {
                                    form.leader = None;
                                }
                            }
                            KeyCode::Char('l') if role.selected => form.leader = Some(role.id.clone()),
                            KeyCode::Char('p') if role.selected && role.project.is_some() => {
                                role.reuse_project = !role.reuse_project;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {
                    if let Some(role) = form.roles.get_mut(form.cursor).filter(|role| role.selected) {
                        role.instructions.key(key);
                    }
                }
            },
        }
        Some(Overlay::StageGroup(form))
    }

    /// 使用原请求 ID 保存，失败后保持表单可重试。
    fn save_stage_group(&mut self, form: &mut StageGroupForm) {
        let Some(lead) = form
            .leader
            .as_ref()
            .filter(|_| [&form.name, &form.goal, &form.acceptance].iter().all(|field| !field.text.trim().is_empty()))
        else {
            form.error = Some("Enter a name, goal, acceptance criteria, and choose a stage lead.".into());
            return;
        };
        if form.from_stage && !form.use_handoff {
            form.error = Some("Review and accept a handoff in Stage details first.".into());
            return;
        }
        let roles: Vec<_> = form
            .roles
            .iter()
            .filter(|role| role.selected)
            .map(|role| json!({"botId":role.id,"sourceRev":role.source_rev,"description":role.instructions.text,"reuseProject":role.reuse_project}))
            .collect();
        form.saving = true;
        form.error = None;
        self.call(
            "createStageGroup",
            json!({
                "botId":form.source, "requestId":form.request_id, "name":form.name.text, "goal":form.goal.text,
                "acceptance":form.acceptance.text, "leaderId":lead, "roles":roles,
                "handoffVersion":form.handoff.as_ref().filter(|_| form.use_handoff).map(|file| &file.id)
            }),
            After::StageGroupCreated,
        );
    }

    /// 新群回复到达后关闭表单并打开聊天，首条任务仍须用户发送。
    pub(super) fn receive_stage_group(&mut self, result: &Value) {
        if let Some(index) = self.overlays.iter().rposition(|overlay| matches!(overlay, Overlay::StageGroup(_))) {
            self.overlays.truncate(index);
        }
        let bot = &result["bot"];
        if let Some(id) = bot["id"].as_str()
            && bot["lastMessage"].is_null()
            && let Some(draft) = result["draft"].as_str()
        {
            self.drafts.entry(id.into()).or_insert_with(|| Editor::with(draft));
        }
        self.receive_saved_bot(bot, true);
    }
}

impl App {
    /// 详情读取不启动群执行，初次读取只填入已保存交接或模板。
    pub(super) fn open_stage_details(&mut self) {
        let Some(room) = self.stage_source().filter(|bot| bot.stage.is_some()).map(|bot| bot.id.clone()) else {
            return;
        };
        self.call("stageDetails", json!({"botId":room}), After::StageDetails { room: room.clone(), saved: false });
        self.overlays.push(Overlay::StageDetails(Box::new(StageDetailsForm {
            room,
            busy: true,
            ..Default::default()
        })));
    }

    /// 快捷键发送明确阶段操作，编辑或历史查看不会自动验收或关闭。
    pub(super) fn stage_details_key(&mut self, mut form: Box<StageDetailsForm>, key: KeyEvent) -> Option<Overlay> {
        if key.code == KeyCode::Esc {
            return None;
        }
        if form.busy {
            return Some(Overlay::StageDetails(form));
        }
        let room = form.room.clone();
        let closed = form.closed();
        let state = form.state.as_ref().unwrap_or(&Value::Null);
        let handoff = &state["bot"]["stage"]["handoff"];
        let version = &handoff["file"]["id"];
        let changed = form.markdown.text.trim() != state["markdown"].as_str().unwrap_or_default()
            || form.evidence.text.trim() != handoff["evidence"].as_str().unwrap_or_default();
        let action = match key.code {
            KeyCode::F(8) if !closed && state["sharedFiles"].as_array().is_some_and(|items| !items.is_empty()) => {
                form.page = if form.page == StageDetailsPage::Evidence {
                    StageDetailsPage::Handoff
                } else {
                    StageDetailsPage::Evidence
                };
                form.evidence_index = 0;
                None
            }
            KeyCode::Up | KeyCode::Down if form.page == StageDetailsPage::Evidence => {
                let count = state["sharedFiles"].as_array().map_or(0, Vec::len);
                form.evidence_index = if key.code == KeyCode::Up {
                    form.evidence_index.saturating_sub(1)
                } else {
                    (form.evidence_index + 1).min(count.saturating_sub(1))
                };
                None
            }
            KeyCode::Enter if form.page == StageDetailsPage::Evidence => {
                if let Some(id) = state["sharedFiles"][form.evidence_index]["id"].as_str() {
                    let reference = format!("file:{id}");
                    if !form.evidence.text.lines().any(|line| line == reference) {
                        let text = form.evidence.text.trim_end();
                        form.evidence =
                            Editor::with(&if text.is_empty() { reference } else { format!("{text}\n{reference}") });
                    }
                }
                form.page = StageDetailsPage::Handoff;
                form.evidence_focused = true;
                None
            }
            KeyCode::F(7) if form.state.is_some() => {
                form.page = if form.page == StageDetailsPage::Usage {
                    StageDetailsPage::Handoff
                } else {
                    StageDetailsPage::Usage
                };
                if form.page == StageDetailsPage::Usage {
                    form.busy = true;
                    form.error = None;
                    self.call("stageUsage", json!({"botId":room}), After::StageUsage(room));
                }
                return Some(Overlay::StageDetails(form));
            }
            KeyCode::F(2) if state["incomingMarkdown"].is_string() => {
                form.page = if form.page == StageDetailsPage::Incoming {
                    StageDetailsPage::Handoff
                } else {
                    StageDetailsPage::Incoming
                };
                None
            }
            KeyCode::F(3) => {
                form.busy = true;
                self.call("stageDetails", json!({"botId":room}), After::StageDetails { room, saved: false });
                return Some(Overlay::StageDetails(form));
            }
            KeyCode::F(4) if version.is_string() && version.as_str() == form.base_version.as_deref() && !changed => {
                Some((json!({"botId":room,"action":"accept","version":version}), false))
            }
            KeyCode::F(5) if form.state.is_some() => {
                Some((json!({"botId":room,"action":if closed {"reopen"} else {"close"}}), false))
            }
            KeyCode::F(6) if !closed && state["bot"]["lastMessage"].is_null() => {
                if let Some(draft) = state["initialDraft"].as_str() {
                    self.drafts.entry(room).or_insert_with(|| Editor::with(draft));
                    return None;
                }
                None
            }
            KeyCode::Char('s')
                if !closed
                    && form.page == StageDetailsPage::Handoff
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Some((
                    json!({"botId":room,"action":"saveHandoff","markdown":form.markdown.text,"evidence":form.evidence.text,
                    "previousVersion":form.base_version}),
                    true,
                ))
            }
            KeyCode::Tab if form.page == StageDetailsPage::Handoff => {
                form.evidence_focused = !form.evidence_focused;
                None
            }
            KeyCode::Up | KeyCode::Down => {
                super::editor::move_line(form.editor(), if key.code == KeyCode::Up { -1 } else { 1 });
                None
            }
            _ if !closed && form.page == StageDetailsPage::Handoff => {
                if key.code == KeyCode::Enter {
                    form.editor().insert("\n");
                } else {
                    form.editor().key(key);
                }
                None
            }
            _ => None,
        };
        if let Some((body, saved)) = action {
            form.error = None;
            form.busy = true;
            self.call("changeStage", body, After::StageDetails { room, saved });
        }
        Some(Overlay::StageDetails(form))
    }

    /// 汇总来自 Host，终端只翻译标题并格式化数值和字段覆盖比例。
    pub(super) fn receive_stage_usage(&mut self, room: &str, result: &Value) {
        use super::super::l10n::tr;
        use std::fmt::Write as _;
        let Some(Overlay::StageDetails(form)) = self
            .overlays
            .last_mut()
            .filter(|overlay| matches!(overlay, Overlay::StageDetails(form) if form.room == room))
        else {
            return;
        };
        let mut text = String::new();
        if let Some(date) = result["observedSince"].as_i64().and_then(chrono::DateTime::from_timestamp_millis) {
            let _ = writeln!(
                text,
                "{}",
                tr("Recording since {date}").replace("{date}", &date.format("%Y-%m-%d").to_string())
            );
        }
        if result["stageCreatedAt"].as_i64() < result["observedSince"].as_i64() {
            let _ = writeln!(text, "{}", tr("Earlier attempts may be missing."));
        }
        let _ = writeln!(text, "{}", tr("Reported fields may overlap. Account usage is separate."));
        for section in result["sections"].as_array().into_iter().flatten() {
            let label = tr(section["label"].as_str().unwrap_or_default());
            let backend = section["backend"].as_str().unwrap_or_default();
            if backend.is_empty() {
                let _ = writeln!(text, "\n{label}");
            } else {
                let unknown = tr("Unknown model");
                let model = section["model"].as_str().unwrap_or(&unknown);
                let _ = writeln!(text, "\n{label} · {backend} · {model}");
            }
            for row in section["rows"].as_array().into_iter().flatten() {
                let label = tr(row["label"].as_str().unwrap_or_default());
                let value =
                    row["value"].as_i64().map_or_else(|| tr("Unavailable").into_owned(), |number| number.to_string());
                let coverage = row["reports"].as_i64().map_or_else(String::new, |reports| {
                    format!(" ({reports}/{})", row["attempts"].as_i64().unwrap_or_default())
                });
                let _ = writeln!(text, "{label} · {value}{coverage}");
            }
        }
        form.usage = Editor::with(&text);
        form.usage.cursor = 0;
        form.busy = false;
    }

    /// 响应只更新对应详情，关闭弹层期间完成的操作仍刷新群的状态。
    pub(super) fn receive_stage_details(&mut self, room: &str, result: Value, saved: bool) {
        self.receive_saved_bot(&result["bot"], false);
        if let Some(Overlay::StageDetails(form)) = self.overlays.last_mut()
            && form.room == room
        {
            if saved || form.state.is_none() || result["bot"]["stage"]["closedAt"].is_number() {
                form.markdown = Editor::with(
                    result["markdown"].as_str().or_else(|| result["template"].as_str()).unwrap_or_default(),
                );
                form.markdown.cursor = 0;
                form.evidence =
                    Editor::with(result["bot"]["stage"]["handoff"]["evidence"].as_str().unwrap_or_default());
                form.base_version = result["bot"]["stage"]["handoff"]["file"]["id"].as_str().map(str::to_owned);
            }
            form.incoming = Editor::with(result["incomingMarkdown"].as_str().unwrap_or_default());
            form.incoming.cursor = 0;
            form.state = Some(result);
            form.busy = false;
            form.error = None;
        }
    }

    /// 阶段操作被拒绝后保留编辑及最近的状态。
    pub(super) fn stage_details_error(&mut self, room: &str, error: String) {
        if let Some(Overlay::StageDetails(form)) = self.overlays.last_mut()
            && form.room == room
        {
            form.busy = false;
            form.error = Some(error);
        }
    }
}
