//! Overlays over the dimmed screen, and their text fields.

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use super::super::app::{App, ConfirmAct, Editor, Overlay, tilde};
use super::super::md::{truncate, width as w};
use super::buffer::{centered, dim_all, frame_box, put, restyle, rput, u};
use super::forms::{form_view, group_view};
use super::goto::goto;
use super::help::help;
use super::new_bot::new_bot_view;
use super::pair::pair;
use super::set_cursor;
use super::stage_group::{stage_details_view, stage_group_view};
use super::style::{rgb, theme};
use super::usage::{usage, usage_chooser};

pub(super) fn overlays(f: &mut Frame<'_>, app: &mut App) {
    if app.overlays.is_empty() {
        return;
    }
    let area = f.area();
    let buf = f.buffer_mut();
    dim_all(buf);
    app.hits.clicks.clear();
    // Only the top overlay is live; draw it alone over the dimmed screen.
    let ovs = std::mem::take(&mut app.overlays);
    if let Some(top) = ovs.last() {
        overlay(buf, area, app, top);
    }
    app.overlays = ovs;
}

pub(super) fn overlay(buf: &mut Buffer, area: Rect, app: &mut App, top: &Overlay) {
    match top {
        Overlay::Goto(g) => {
            let items = app.goto_items(g);
            goto(buf, area, app, g, &items);
        }
        Overlay::GroupGuidance { targets, cursor } => {
            // 复用终端弹层和选中行样式，只显示可引导的成员。
            let t = theme();
            let r = centered(area, 48, u(targets.len().saturating_add(5)).min(24));
            let inner = frame_box(buf, r, t.text, t.panel, Some(("引导成员", t.text)));
            let rows = usize::from(inner.height.saturating_sub(2));
            let offset = cursor.saturating_sub(rows.saturating_sub(1));
            for (row, (_, name)) in targets.iter().skip(offset).take(rows).enumerate() {
                let y = inner.y + u(row);
                let selected = row + offset == *cursor;
                let style = if selected { t.bold.patch(t.sel) } else { t.text.patch(t.panel) };
                if selected {
                    restyle(buf, Rect::new(inner.x, y, inner.width, 1), t.sel);
                }
                put(buf, inner.x, y, inner.width, name, style);
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(1),
                inner.width,
                "↑↓ 选择  ↵ 引导  esc 取消",
                t.dim.patch(t.panel),
            );
        }
        Overlay::GroupWorkspaces { usage, cursor, .. } => {
            super::usage::group_workspaces(buf, area, usage.as_ref(), *cursor, app.on_this_computer);
        }
        Overlay::SessionHistory { snapshot, messages, cursor, .. } => {
            session_history(buf, area, snapshot, messages.as_ref(), *cursor);
        }
        Overlay::MemberActivity { bot, room, snapshot, result, cursor } => {
            let row = app.member_activity_row(room, snapshot);
            let title = format!(
                "{} · {}",
                super::super::l10n::tr(if result.is_some() { "View result" } else { "Member activity" }),
                app.author_name(Some(bot))
            );
            let mut hint = "↑↓  esc".to_owned();
            if result.is_none() {
                if snapshot["entry"]["id"].as_str().is_some_and(|id| app.lane(room).iter().any(|entry| entry.id == id))
                {
                    let _ = write!(hint, "  o {}", super::super::l10n::tr("Open original assignment"));
                }
                if row.is_some_and(|row| row["resultId"].is_string()) {
                    let _ = write!(hint, "  ↵ {}", super::super::l10n::tr("View result"));
                }
                if app.session_history
                    && app.group_contexts.as_ref().is_some_and(|(chat, snapshot)| {
                        chat == room
                            && snapshot["members"].as_array().is_some_and(|members| {
                                members.iter().any(|member| member["botId"] == bot.as_str() && member["shared"] != true)
                            })
                    })
                {
                    let _ = write!(hint, "  H {}", super::super::l10n::tr("Session history"));
                }
            }
            let text = result.clone().unwrap_or_else(|| {
                row.map_or_else(
                    || {
                        super::super::l10n::tr(if snapshot.is_null() { "Loading…" } else { "No assignments yet" })
                            .into_owned()
                    },
                    |row| {
                        let mut lines = vec![
                            super::super::app::Entry::delivery_status(row["status"].as_str().unwrap_or_default())
                                .to_owned(),
                        ];
                        lines.extend(super::transcript::activity_lines(row));
                        lines.join("\n")
                    },
                )
            });
            let t = theme();
            let inner = frame_box(buf, centered(area, 76, 20), t.text, t.panel, Some((&title, t.text)));
            inspection_text(buf, inner, &text, *cursor, &hint);
        }
        Overlay::Help(filter) => help(buf, area, filter),
        Overlay::LeadAgent(scroll) => super::lead_agent::lead_agent(buf, area, *scroll),
        Overlay::Confirm(c) => {
            let t = theme();
            // 为彻底删除及其范围说明留出额外行，普通确认沿用原高度。
            let purge = matches!(&c.act, ConfirmAct::DeleteGroup(_));
            let r = centered(area, 66, if purge { 11 } else { 8 });
            let inner = frame_box(buf, r, t.red, t.panel, None);
            // The title already includes a bot or device name. Translating it
            // here would also translate a name that is itself a phrase.
            let detail = super::super::l10n::tr(&c.detail);
            let note = super::super::l10n::tr(&c.note);
            let button = super::super::l10n::tr(c.button);
            let cancel = super::super::l10n::tr("Cancel");
            put(buf, inner.x, inner.y, inner.width, &c.title, t.red.patch(t.panel).add_modifier(Modifier::BOLD));
            put(buf, inner.x, inner.y + 1, inner.width, detail.as_ref(), t.text.patch(t.panel));
            put(buf, inner.x, inner.y + 2, inner.width, note.as_ref(), t.secondary.patch(t.panel));
            let danger = if t.color {
                Style::default().fg(t.on_color).bg(rgb(0xF0A7A7)).add_modifier(Modifier::BOLD)
            } else {
                t.sel
            };
            let x = put(buf, inner.x, inner.y + 4, 30, &format!(" ↵ {button} "), danger) + 1;
            put(buf, x, inner.y + 4, 14, &format!(" esc {cancel} "), t.text.patch(t.btn).add_modifier(Modifier::BOLD));
            if purge {
                let button = super::super::l10n::tr("Delete group and its bots");
                let bots = super::super::l10n::tr("Removes exclusive bots, conversations and resources.");
                let copies = super::super::l10n::tr("Copied chats, shared bots and external projects stay.");
                put(buf, inner.x, inner.y + 6, inner.width, &format!(" X {button} "), danger);
                put(buf, inner.x, inner.y + 7, inner.width, &bots, t.text.patch(t.panel));
                put(buf, inner.x, inner.y + 8, inner.width, &copies, t.secondary.patch(t.panel));
            }
        }
        Overlay::NewBot(n) => new_bot_view(buf, area, n),
        Overlay::Folder(p) => {
            let t = theme();
            let matches = App::folder_matches(p);
            let r = centered(area, 74, 22);
            let inner = frame_box(buf, r, t.text, t.panel, Some(("Choose workspace", t.text)));
            let shown = tilde(&p.path, &app.home);
            let no_recent = p.mode == crate::tui::app::FolderMode::Recent && !p.busy && p.recent.is_empty();
            let path = match p.mode {
                crate::tui::app::FolderMode::Recent if no_recent => "^B Browse other folders".to_owned(),
                crate::tui::app::FolderMode::Recent => "Recent workspaces".to_owned(),
                crate::tui::app::FolderMode::Path => "Enter folder path".to_owned(),
                crate::tui::app::FolderMode::Browse => shown.clone(),
            };
            let x = put(
                buf,
                inner.x,
                inner.y,
                inner.width / 2,
                &truncate(&path, usize::from(inner.width / 2)),
                t.secondary.patch(t.panel),
            );
            if !no_recent {
                field_line(buf, Rect::new(x, inner.y, inner.right().saturating_sub(x), 1), inner.y, &p.query, "", true);
            }
            let list_h = usize::from(inner.height.saturating_sub(5));
            let off = p.cursor.saturating_sub(list_h.saturating_sub(1));
            let mut y = inner.y + 2;
            if let Some(e) = &p.error {
                put(buf, inner.x, y, inner.width, e, t.red.patch(t.panel));
                y += 1;
            }
            if p.busy {
                put(buf, inner.x, y, inner.width, "Loading…", t.dim.patch(t.panel));
                y += 1;
            }
            for (i, d) in matches.iter().enumerate().skip(off).take(list_h) {
                let sel = i == p.cursor;
                let base = if sel { t.sel } else { t.panel };
                if sel {
                    restyle(buf, Rect::new(inner.x - 1, y, inner.width + 2, 1), t.sel);
                }
                put(buf, inner.x, y, 2, "▸", t.secondary.patch(base));
                put(
                    buf,
                    inner.x + 2,
                    y,
                    inner.width - 10,
                    &format!("{}/", d.name),
                    if sel { t.bold } else { t.text }.patch(base),
                );
                if d.git {
                    rput(buf, inner.right(), y, "git", t.dim.patch(base));
                }
                y += 1;
            }
            let chosen = matches.get(p.cursor).map_or(shown.clone(), |d| tilde(&d.path, &app.home));
            put(
                buf,
                inner.x,
                inner.bottom() - 2,
                inner.width,
                &format!("↵ use {}", truncate(&chosen, usize::from(inner.width.saturating_sub(8)))),
                t.text.patch(t.panel),
            );
            put(
                buf,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                "→ open  ← up  . use current  ^R recent  ^B browse  ^L path",
                t.dim.patch(t.panel),
            );
        }
        Overlay::Form(form) => form_view(buf, area, app, form),
        Overlay::Group(g) => group_view(buf, area, app, g),
        Overlay::StageGroup(g) => stage_group_view(buf, area, app, g),
        Overlay::StageDetails(g) => stage_details_view(buf, area, g, app.stage_roster),
        Overlay::DeliveryControls { tasks, cursor, .. } => {
            super::activity::delivery_controls(buf, area, app, tasks, *cursor);
        }
        Overlay::Usage(scroll) => usage(buf, area, app, *scroll),
        Overlay::UsageChooser { row, .. } => usage_chooser(buf, area, app, *row),
        Overlay::Pair(code) => pair(buf, area, app, code),
        Overlay::Memory(m) => super::super::sheets::memory(buf, area, app, m),
        Overlay::BotChat(c) => super::bot_chat::bot_chat(buf, area, app, c),
        Overlay::DevServers(sheet) => super::dev_servers::dev_servers(buf, area, app, sheet),
        Overlay::Routines(l) => super::super::sheets::routines(buf, area, app, l),
        Overlay::Routine(f) => super::super::sheets::routine(buf, area, f),
        Overlay::Library(l) => super::super::sheets::library(buf, area, app, l),
        Overlay::Gear(g) => super::super::sheets::gear(buf, area, app, g),
        Overlay::Market(m) => super::super::sheets::market(buf, area, app, m),
        Overlay::Agent(a) => super::super::sheets::agent(buf, area, app, a),
        Overlay::Fields(f) => super::super::sheets::fields(buf, area, f),
        Overlay::Skill(s) => super::skill::skill(buf, area, app, s),
        Overlay::Analytics(s) => super::analytics::analytics(buf, area, s),
        Overlay::Network(n) => super::network::network(buf, area, n),
    }
}

/// A one-line text field. A search shows "/ " first, unless the caller drew its own prefix
/// (the folder path, with an empty placeholder).
pub(in crate::tui) fn field_line(buf: &mut Buffer, r: Rect, y: u16, e: &Editor, placeholder: &str, active: bool) {
    input_line(buf, r, y, e, placeholder, active, !placeholder.is_empty());
}

/// 绘制历史列表、当前会话及只读聊天预览。
fn session_history(
    buf: &mut Buffer,
    area: Rect,
    snapshot: &serde_json::Value,
    messages: Option<&serde_json::Value>,
    cursor: usize,
) {
    use super::super::l10n::tr;
    let t = theme();
    let title = tr(if messages.is_some() { "Chat history" } else { "Session history" });
    let inner = frame_box(buf, centered(area, 76, 20), t.text, t.panel, Some((&title, t.text)));
    if let Some(messages) = messages {
        let text = messages["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|message| {
                let name = tr(if message["kind"] == "user" { "You" } else { "Bot" });
                format!(
                    "{name} · {}\n{}",
                    super::time::clock(message["at"].as_i64().unwrap_or_default()),
                    message["text"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        inspection_text(buf, inner, &text, cursor, "↑↓  esc");
        return;
    }
    let active = tr(if snapshot.is_null() {
        "Loading…"
    } else if snapshot["active"] == true {
        "In use"
    } else {
        "No session yet"
    });
    put(buf, inner.x, inner.y, inner.width, &format!("{} · {active}", tr("Current session")), t.bold.patch(t.panel));
    let rows = usize::from(inner.height.saturating_sub(2));
    let sessions = snapshot["sessions"].as_array().map(Vec::as_slice).unwrap_or_default();
    let offset = cursor.saturating_sub(rows.saturating_sub(1));
    for (index, item) in sessions.iter().enumerate().skip(offset).take(rows) {
        let state = tr(match item["state"].as_str() {
            Some("archived") => "Archived",
            Some("retained") => "Retained",
            Some("deleted") => "Only chat history remains",
            _ => "Archive status unknown",
        });
        let time = chrono::DateTime::from_timestamp_millis(item["at"].as_i64().unwrap_or_default())
            .map_or_else(String::new, |at| at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
        let text = format!("{time} · {state}");
        let style = if index == cursor { t.bold.patch(t.sel) } else { t.text.patch(t.panel) };
        put(buf, inner.x, inner.y + 1 + u(index - offset), inner.width, &text, style);
    }
    if !snapshot.is_null() && sessions.is_empty() {
        put(buf, inner.x, inner.y + 1, inner.width, &tr("No old sessions"), t.dim.patch(t.panel));
    }
    let selected = sessions.get(cursor);
    let mut hint = String::from("↑↓  esc");
    if selected.is_some_and(|item| item["hasMessages"] == true) {
        let _ = write!(hint, "  ↵ {}", tr("View chat history"));
    }
    if selected.is_some_and(|item| item["canDelete"] == true) {
        let _ = write!(hint, "  D {}", tr("Delete old agent session"));
    }
    put(buf, inner.x, inner.bottom().saturating_sub(1), inner.width, &hint, t.dim.patch(t.panel));
}

/// 按可见高度分页显示只读文本，避免长回复撑破终端。
fn inspection_text(buf: &mut Buffer, inner: Rect, text: &str, cursor: usize, hint: &str) {
    use ratatui::text::Span;
    let t = theme();
    let lines = text
        .lines()
        .flat_map(|line| super::super::md::wrap(&[Span::raw(line.to_owned())], usize::from(inner.width), &[], &[]))
        .collect::<Vec<_>>();
    let height = usize::from(inner.height.saturating_sub(1));
    let offset = cursor.min(lines.len().saturating_sub(height));
    for (index, line) in lines.into_iter().skip(offset).take(height).enumerate() {
        buf.set_line(inner.x, inner.y + u(index), &line.style(t.text.patch(t.panel)), inner.width);
    }
    put(buf, inner.x, inner.bottom().saturating_sub(1), inner.width, hint, t.dim.patch(t.panel));
}

pub(in crate::tui) fn input_line(
    buf: &mut Buffer,
    r: Rect,
    y: u16,
    e: &Editor,
    placeholder: &str,
    active: bool,
    search: bool,
) {
    let t = theme();
    let x = if search { put(buf, r.x, y, 2, "/ ", t.dim.patch(t.panel)) } else { r.x };
    if e.text.is_empty() {
        if active {
            set_cursor(Position::new(x, y));
        }
        let hint = super::super::l10n::tr(placeholder);
        put(buf, x, y, r.right().saturating_sub(x), hint.as_ref(), t.dim.patch(t.panel));
    } else {
        let room = usize::from(r.right().saturating_sub(x + 1));
        let text = e.text.replace('\n', " ");
        let before = w(&e.text[..e.cursor]);
        let skip = before.saturating_sub(room);
        let shown: String = text.chars().skip(skip).collect();
        put(buf, x, y, u(room), &shown, t.bold.patch(t.panel));
        if active {
            set_cursor(Position::new(x + u(before - skip), y));
        }
    }
}
