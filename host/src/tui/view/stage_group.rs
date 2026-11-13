//! 终端阶段预览显示当前角色的工作目录、负责人和记忆边界。

use super::super::app::{App, StageDetailsForm, StageDetailsPage, StageGroupForm};
use super::super::l10n::tr;
use super::buffer::{centered, frame_box, put, u};
use super::overlay::input_line;
use super::style::theme;
use ratatui::{buffer::Buffer, layout::Rect};

/// 绘制可滚动角色列表及当前角色的编辑字段。
pub(super) fn stage_group_view(buf: &mut Buffer, area: Rect, app: &App, stage: &StageGroupForm) {
    let t = theme();
    let title = tr("Create next stage group");
    let inner = frame_box(buf, centered(area, 83, 22), t.text, t.panel, Some((title.as_ref(), t.text)));
    if inner.height < 13 || inner.width < 30 {
        return;
    }
    let column = 31.min(inner.width - 12);
    for (field, y, key, editor) in [
        (0, inner.y, "Stage name", &stage.name),
        (1, inner.y + 1, "Search or add bots", &stage.query),
        (
            4,
            inner.bottom() - 7,
            if app.group_member_picker { "Stage goal (optional)" } else { "Stage goal" },
            &stage.goal,
        ),
        (
            5,
            inner.bottom() - 6,
            if app.group_member_picker { "Acceptance criteria (optional)" } else { "Acceptance criteria" },
            &stage.acceptance,
        ),
    ] {
        put(buf, inner.x, y, column, tr(key).as_ref(), t.secondary.patch(t.panel));
        input_line(
            buf,
            Rect::new(inner.x + column, y, inner.width - column, 1),
            y,
            editor,
            "",
            stage.field == field,
            false,
        );
    }
    let indices = stage.role_indices(app);
    let rows = usize::from(inner.height - 11);
    let current = indices.iter().position(|index| *index == stage.cursor).unwrap_or_default();
    let offset = current.saturating_sub(rows.saturating_sub(1));
    if indices.is_empty() {
        put(buf, inner.x, inner.y + 3, inner.width, tr("No matching bots").as_ref(), t.dim);
    }
    for (line, (position, index)) in indices.iter().enumerate().skip(offset).take(rows).enumerate() {
        let role = &stage.roles[*index];
        let name = app.bots.get(&role.id).map_or(role.id.as_str(), |bot| bot.name.as_str());
        let selected = if role.selected { "☑" } else { "☐" };
        let style = if stage.field == 2 && current == position { t.bold.patch(t.sel) } else { t.text.patch(t.panel) };
        put(buf, inner.x, inner.y + 3 + u(line), inner.width, &format!("{selected} {name}"), style);
    }
    let lead = stage.leader.as_ref().and_then(|id| app.bots.get(id)).map_or_else(
        || tr(if app.optional_stage_setup { "No lead" } else { "Choose" }).into_owned(),
        |bot| bot.name.clone(),
    );
    put(
        buf,
        inner.x,
        inner.bottom() - 8,
        inner.width,
        &format!("{}   ‹ {lead} ›", tr(if app.optional_stage_setup { "Group lead (optional)" } else { "Group lead" })),
        if stage.field == 3 { t.bold.patch(t.sel) } else { t.text.patch(t.panel) },
    );
    if let Some(role) = indices.get(current).and_then(|index| stage.roles.get(*index)) {
        let y = inner.bottom() - 5;
        let workspace = tr("Personal workspace");
        let folder = if role.reuse_project { role.project.as_deref().unwrap_or_default() } else { &workspace };
        put(buf, inner.x, y, inner.width, folder, t.secondary.patch(t.panel));
        let memory = tr(if role.shared_memory {
            "This agent shares its own memory across sessions."
        } else {
            "New bots start with their own workspace and Codync shared memory off."
        });
        put(buf, inner.x, y + 1, inner.width, &memory, t.secondary.patch(t.panel));
        put(buf, inner.x, y + 2, 21, tr("Role instructions").as_ref(), t.secondary.patch(t.panel));
        input_line(
            buf,
            Rect::new(inner.x + 21, y + 2, inner.width - 21, 1),
            y + 2,
            &role.instructions,
            "",
            stage.field == 6 && role.selected,
            false,
        );
    }
    if stage.from_stage && (stage.handoff.is_some() || !app.optional_stage_setup) {
        let summary = stage.handoff.as_ref().map_or_else(
            || tr("Review and accept a handoff in Stage details first.").into_owned(),
            |file| {
                format!(
                    "F2 {} {} · {}",
                    if stage.use_handoff { "☑" } else { "☐" },
                    tr("Use accepted handoff"),
                    file.sha256.get(..12).unwrap_or(&file.sha256)
                )
            },
        );
        put(buf, inner.x, inner.bottom() - 2, inner.width, &summary, t.dim);
    }
    let label = tr(if stage.saving {
        "Creating…"
    } else {
        "tab next field · space select · ←→ lead · p project · ^s create"
    });
    let text = stage.error.as_deref().unwrap_or(&label);
    put(buf, inner.x, inner.bottom() - 1, inner.width, text, if stage.error.is_some() { t.red } else { t.dim });
}

/// 阶段交接使用聊天编辑器的多行换行规则，已确认内容可以用上下键查看。
pub(super) fn stage_details_view(buf: &mut Buffer, area: Rect, form: &StageDetailsForm, stage_roster: bool) {
    use ratatui::layout::Position;
    let t = theme();
    let title = tr("Stage details");
    let inner = frame_box(buf, centered(area, 92, 27), t.text, t.panel, Some((title.as_ref(), t.text)));
    if inner.height < 12 || inner.width < 26 {
        return;
    }
    if let Some(state) = &form.state {
        let stage = &state["bot"]["stage"];
        let stage_state = tr(if form.archived() {
            "Stage archived"
        } else if form.closed() {
            "Stage closed"
        } else {
            "Stage open"
        });
        let accepted =
            tr(if stage["handoff"]["acceptedAt"].is_number() { "Stage accepted" } else { "Stage awaiting acceptance" });
        let top = format!("{} · {stage_state} · {accepted}", state["bot"]["name"].as_str().unwrap_or_default());
        put(buf, inner.x, inner.y, inner.width, &top, t.bold.patch(t.panel));
        let counts = state["work"]["counts"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(status, count)| format!("{} · {count}", tr(status)))
            .collect::<Vec<_>>()
            .join("  ");
        put(buf, inner.x, inner.y + 1, inner.width, &counts, t.secondary.patch(t.panel));
        let pending = state["work"]["pending"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| {
                format!(
                    "{} · {}",
                    item["name"].as_str().unwrap_or_default(),
                    tr(item["status"].as_str().unwrap_or_default())
                )
            })
            .collect::<Vec<_>>()
            .join("  ");
        put(buf, inner.x, inner.y + 2, inner.width, &pending, t.secondary.patch(t.panel));
        if form.page == StageDetailsPage::Evidence {
            let height = usize::from(inner.height - 6);
            let first = form.evidence_index.saturating_sub(height - 1);
            put(buf, inner.x, inner.y + 3, inner.width, &tr("Shared evidence files"), t.dim.patch(t.panel));
            for (row, file) in
                state["sharedFiles"].as_array().into_iter().flatten().skip(first).take(height).enumerate()
            {
                let style =
                    if row + first == form.evidence_index { t.bold.patch(t.sel) } else { t.text.patch(t.panel) };
                let hash = file["sha256"].as_str().unwrap_or_default();
                let label =
                    format!("{} · {}", file["name"].as_str().unwrap_or_default(), hash.get(..12).unwrap_or(hash));
                put(buf, inner.x, inner.y + 4 + u(row), inner.width, &label, style);
            }
            put(
                buf,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &format!("↑ ↓ · ↵ {} · F8", tr("Add evidence file")),
                t.dim,
            );
            return;
        }
    }
    let caption = if form.page == StageDetailsPage::Usage {
        "Stage usage"
    } else if form.page == StageDetailsPage::Incoming {
        "Incoming handoff"
    } else {
        "Handoff Markdown"
    };
    put(buf, inner.x, inner.y + 3, inner.width, &tr(caption), t.dim.patch(t.panel));
    let editor = if form.page == StageDetailsPage::Usage {
        &form.usage
    } else if form.page == StageDetailsPage::Incoming {
        &form.incoming
    } else {
        &form.markdown
    };
    let height = usize::from(inner.height - 9);
    let (lines, (x, y)) = super::chat::composer_lines(editor, usize::from(inner.width.saturating_sub(2)));
    let first = y.saturating_sub(height - 1);
    for (row, line) in lines.iter().skip(first).take(height).enumerate() {
        put(buf, inner.x, inner.y + 4 + u(row), inner.width, line, t.text.patch(t.panel));
    }
    if !form.closed() && form.page == StageDetailsPage::Handoff {
        input_line(
            buf,
            inner,
            inner.bottom() - 4,
            &form.evidence,
            "Durable evidence references",
            form.evidence_focused,
            false,
        );
        if !form.evidence_focused {
            super::set_cursor(Position::new(inner.x + u(x), inner.y + 4 + u(y - first)));
        }
    } else if form.closed() && form.page == StageDetailsPage::Handoff {
        put(buf, inner.x, inner.bottom() - 4, inner.width, &form.evidence.text, t.secondary.patch(t.panel));
    }
    if let Some(error) = &form.error {
        put(buf, inner.x, inner.bottom() - 3, inner.width, error, t.red.patch(t.panel));
    } else if form.state.as_ref().is_some_and(|v| v["handoffUnavailable"] == true || v["incomingUnavailable"] == true) {
        put(
            buf,
            inner.x,
            inner.bottom() - 3,
            inner.width,
            &tr("The handoff file is unavailable."),
            t.red.patch(t.panel),
        );
    }
    let close = tr(if form.closed() { "Reopen stage" } else { "Close stage" });
    let keys = if form.archived() {
        format!("F2 {} · F3 {} · F9 {}", tr("Incoming handoff"), tr("Refresh"), tr("Restore stage"))
    } else {
        let archive = if stage_roster { format!(" · F9 {}", tr("Archive stage")) } else { String::new() };
        format!(
            "F2 {} · F3 {} · F4 {} · F5 {close}{archive}",
            tr("Incoming handoff"),
            tr("Refresh"),
            tr("Accept stage")
        )
    };
    put(buf, inner.x, inner.bottom() - 2, inner.width, &keys, t.dim.patch(t.panel));
    let restore =
        if form.first_draft().is_some() { format!(" · F6 {}", tr("Restore first draft")) } else { String::new() };
    let edits =
        format!("F7 {} · F8 {} · ^S {}{restore}", tr("Stage usage"), tr("Shared evidence files"), tr("Save handoff"));
    put(buf, inner.x, inner.bottom() - 1, inner.width, &edits, t.dim.patch(t.panel));
}
