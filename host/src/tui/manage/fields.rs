//! The labeled text fields sheet: keys, connector setup, a pasted MCP config, a handoff.

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};

use super::super::app::{App, Editor, Entry, Overlay};
use super::super::l10n::tr;
use super::market::connector_inputs;
use super::{Fields, Input, Reply, Submit, ctrl, newline};

impl App {
    /// 只更新当前群的会话面板，关闭或切换群后忽略晚到结果。
    pub(in crate::tui) fn show_group_resource_usage(&mut self, room: &str, usage: Option<&Value>) {
        let local = self.on_this_computer;
        // 工作区子页已打开时仍更新背后的同群资料，返回后能看到附件和 Token。
        let details = self.overlays.iter_mut().rev().find_map(|overlay| {
            let Overlay::Fields(fields) = overlay else { return None };
            let Submit::GroupContextOptions { room: viewed, directory, usage_notes, .. } = &mut fields.submit else {
                return None;
            };
            (viewed == room).then_some((directory, usage_notes, &mut fields.choices))
        });
        let Some((directory, usage_notes, choices)) = details else { return };
        let Some(usage) = usage else {
            *usage_notes = vec![tr("Could not refresh usage.").into_owned()];
            return;
        };
        // 未知数字保留破折号，群组及 Host 的范围来自同一快照。
        let number = |field: &str| usage[field].as_u64().map_or_else(|| "—".into(), |n| n.to_string());
        let share = usage["share"].as_f64().map_or_else(|| "—".into(), |n| format!("{:.1}%", n * 100.0));
        *usage_notes = vec![
            format!("{} · {} B", tr("Group attachments and shared files"), number("resourceBytes")),
            format!("{} · {} Token", tr("Group token usage"), number("groupTokens")),
            format!("{} · {share}", tr("Share of host usage")),
            tr("All conversations on this host: {tokens} Token").replace("{tokens}", &number("hostTokens")),
        ];
        if usage["partial"] == true {
            usage_notes.push(tr("Partial usage records").into_owned());
        }
        if local && directory.is_none() {
            *directory = usage["resourceDirectory"].as_str().map(str::to_owned);
            if directory.is_some() {
                choices.push(tr("Open folder").into_owned());
            }
        }
    }

    /// 更新同一个群的合计和已展开列表，离开群后忽略结果。
    pub(in crate::tui) fn show_group_workspace_usage(&mut self, room: &str, report: &Value) {
        for overlay in &mut self.overlays {
            match overlay {
                Overlay::Fields(fields) => {
                    if let Submit::GroupContextOptions { room: viewed, workspaces, .. } = &mut fields.submit
                        && viewed == room
                    {
                        *workspaces = Some(report.clone());
                    }
                }
                Overlay::GroupWorkspaces { room: viewed, usage, .. } if viewed == room => {
                    *usage = Some(report.clone());
                }
                _ => {}
            }
        }
    }

    /// 处理详情选择，打开工作区列表时保留背后的群资料。
    pub(in crate::tui) fn fields_key(&mut self, mut f: Box<Fields>, k: KeyEvent) -> Option<Overlay> {
        if f.saving {
            return if k.code == KeyCode::Esc { None } else { Some(Overlay::Fields(f)) };
        }
        let rows = f.offset() + f.inputs.len();
        let on_choice = f.offset() == 1 && f.cursor == 0;
        let multiline = f.current().is_some_and(|i| i.multiline);
        match k.code {
            KeyCode::Esc => return None,
            _ if newline(k) && multiline => {
                if let Some(i) = f.current() {
                    i.ed.insert("\n");
                }
            }
            KeyCode::Enter => self.save_fields(&mut f),
            KeyCode::Char('s') if ctrl(k) => self.save_fields(&mut f),
            KeyCode::Tab | KeyCode::Down => f.cursor = (f.cursor + 1) % rows.max(1),
            KeyCode::BackTab | KeyCode::Up => f.cursor = (f.cursor + rows.max(1) - 1) % rows.max(1),
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if on_choice => {
                f.choice = (f.choice + 1) % f.choices.len();
                connector_inputs(&mut f);
                super::super::connections::option_fields(&mut f);
            }
            _ => {
                if let Some(i) = f.current() {
                    i.ed.key(k);
                }
            }
        }
        if f.saving && matches!(f.submit, Submit::GroupContextOptions { .. }) { None } else { Some(Overlay::Fields(f)) }
    }

    /// 保存当前编辑内容，独立群交接一次提交编辑与确认。
    fn save_fields(&mut self, f: &mut Fields) {
        if f.saving {
            return;
        }
        if let Some(i) = f.inputs.iter().find(|i| i.required && i.ed.text.trim().is_empty()) {
            f.error = Some(format!("{} is required.", i.label));
            return;
        }
        let values: serde_json::Map<String, Value> = f
            .inputs
            .iter()
            .filter(|i| !i.ed.text.trim().is_empty())
            .map(|i| (i.name.clone(), if i.secret { i.ed.text.as_str() } else { i.ed.text.trim() }.into()))
            .collect();
        f.saving = true;
        f.error = None;
        match &f.submit {
            Submit::Icon(target) => {
                let mut body = json!({"target": target, "tinted": f.choice == 1});
                if f.choice == 2 {
                    body["remove"] = true.into();
                } else if let Some(source) = values.get("source").and_then(Value::as_str) {
                    let key = if source.starts_with("https://") { "url" } else { "localFile" };
                    body[key] = source.into();
                }
                self.sheet("setIcon", body, Reply::FieldsSaved);
            }
            Submit::Connection(_) => self.submit_connection(f, &values),
            Submit::AgentEnv(backend) => {
                if values.is_empty() {
                    f.saving = false;
                    f.error = Some("Type at least one key.".into());
                    return;
                }
                self.sheet("setAgentEnv", json!({"backend": backend, "vars": values}), Reply::FieldsSaved);
            }
            Submit::Connector(item) => {
                let option = item["options"][f.choice]["id"].clone();
                self.sheet(
                    "installConnector",
                    json!({"registryName": item["name"], "option": option, "inputs": values}),
                    Reply::FieldsSaved,
                );
            }
            Submit::Import => self.sheet("importConnectors", json!({"config": values["config"]}), Reply::FieldsSaved),
            Submit::Handoff(entry) => {
                self.sheet("editHandoff", json!({"entryId": entry, "text": values["text"]}), Reply::FieldsSaved);
            }
            Submit::GroupHandoff { delivery, revision } => {
                self.sheet(
                    "confirmGroupHandoff",
                    json!({"deliveryId":delivery,"revision":revision,"text":values["text"]}),
                    Reply::FieldsSaved,
                );
            }
            Submit::MemberSession { room, bot, revision } => {
                self.sheet(
                    "confirmGroupMemberSession",
                    json!({"roomId":room,"botId":bot,"revision":revision,"text":values["text"]}),
                    Reply::FieldsSaved,
                );
            }
            Submit::GroupContextOptions { room, members, directory, workspaces, workspaces_enabled, .. } => {
                if let Some(basis) = [256_000, 512_000, 1_000_000].get(f.choice) {
                    self.call(
                        "setGroupContextBasis",
                        json!({"roomId":room,"basis":basis}),
                        super::After::GroupContexts(room.clone()),
                    );
                } else if *workspaces_enabled && f.choice == 3 {
                    f.saving = false;
                    self.overlays.push(Overlay::GroupWorkspaces {
                        room: room.clone(),
                        usage: workspaces.clone(),
                        cursor: 0,
                    });
                } else if let Some((bot, action)) = members.get(f.choice - 3 - usize::from(*workspaces_enabled)) {
                    if *action {
                        self.open_member_session(room.clone(), bot.clone());
                    } else {
                        self.inspect_member_context(room, bot);
                    }
                } else if let Some(path) = directory {
                    // 只有本机 Host 会返回可打开的资源目录。
                    use crate::analytics::reported::{FileLinkAction, FileLinkOutcome, Reported};
                    let outcome = if crate::shell::open(std::ffi::OsStr::new(path)) {
                        FileLinkOutcome::Ok
                    } else {
                        self.flash("Could not open the group resource folder.");
                        FileLinkOutcome::Failed
                    };
                    self.track(Reported::FileLinkAction { action: FileLinkAction::Open, outcome });
                }
            }
            Submit::Section(bot) => {
                self.sheet("updateBot", json!({"id": bot, "section": values.get("name")}), Reply::FieldsSaved);
            }
            Submit::RenameSection(from) => {
                self.section_order = self.shown_section_names();
                let to = values.get("name").and_then(Value::as_str).map(str::to_owned);
                let r = Reply::SectionRenamed { from: from.clone(), to: to.clone() };
                self.sheet("renameSection", json!({"from": from, "to": to}), r);
            }
            Submit::RenameArtifact { bot, artifact } => {
                let body = json!({"botId": bot, "artifactId": artifact, "title": values["title"]});
                self.sheet("renameArtifact", body, Reply::FieldsSaved);
            }
        }
    }
}

/// Edits what a handoff card carries into the next conversation.
pub fn handoff_fields(entry: &Entry) -> Fields {
    let group = entry.data["handoff"]["reason"] == "groupCapacity";
    handoff_input(
        entry.data["handoff"]["text"].as_str().unwrap_or_default(),
        Submit::Handoff(entry.id.clone()),
        if group {
            "编辑交接后回到私聊卡确认。⇧↵ 换行，↵ 保存。"
        } else {
            "Your next message starts a fresh conversation that begins with this. ⇧↵ new line, ↵ saves."
        },
    )
}

/// 独立群在编辑面板内确认，原派工收到完整摘要。
pub(super) fn group_handoff_fields(delivery: String, revision: String, text: &str) -> Fields {
    let mut fields = handoff_input(
        text,
        Submit::GroupHandoff { delivery, revision },
        "切换此机器人在当前群的会话。群历史和待办保留。⇧↵ 换行，↵ 确认。",
    );
    fields.title = "Open new session".into();
    fields
}

/// 成员没有阻塞派工时，摘要随下一条属于这个群的任务使用。
pub(super) fn member_session_fields(room: String, bot: String, revision: String, text: &str) -> Fields {
    let mut fields = handoff_input(
        text,
        Submit::MemberSession { room, bot, revision },
        "The new session starts after the current turn. ⇧↵ new line, ↵ confirms.",
    );
    fields.title = "Open new session".into();
    fields
}

/// 交接输入复用同一个多行编辑器。
fn handoff_input(text: &str, submit: Submit, note: &str) -> Fields {
    Fields {
        title: "Edit the handoff".into(),
        note: note.into(),
        choices: vec![],
        choice: 0,
        inputs: vec![Input {
            name: "text".into(),
            label: "What it carries".into(),
            secret: false,
            required: true,
            multiline: true,
            placeholder: String::new(),
            ed: Editor::with(text),
        }],
        cursor: 0,
        error: None,
        saving: false,
        submit,
    }
}

/// Asks for a section name: where a bot goes, or a section's new name. Empty is allowed.
pub fn section_fields(title: &str, note: String, current: Option<&str>, submit: Submit) -> Fields {
    Fields {
        title: title.into(),
        note,
        choices: vec![],
        choice: 0,
        inputs: vec![Input {
            name: "name".into(),
            label: "Section".into(),
            secret: false,
            required: false,
            multiline: false,
            placeholder: "Section name".into(),
            ed: Editor::with(current.unwrap_or_default()),
        }],
        cursor: 0,
        error: None,
        saving: false,
        submit,
    }
}
