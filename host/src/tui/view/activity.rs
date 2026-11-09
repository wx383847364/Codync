//! The current activity and how many steps the turn has taken: with thinking details on, `>` (or a
//! click) unfolds them under it; `t` opens Full conversation.

use ratatui::text::{Line, Span};

use super::super::app::{App, Bot, Click, Entry, Status, running_turn, working_steps};
use super::super::l10n::tr;
use super::super::md::truncate;
use super::buffer::u;
use super::style::{spin, theme};
use super::time::elapsed;
use super::trace::push_step;
use super::transcript::{Built, gap, name_style};

/// The most lines the unfolded steps take; earlier ones stay in Full conversation.
const STEP_LINES: usize = 14;

/// 在会话底部显示正在本群工作的成员，保留终端原有的回合详情入口。
pub(super) fn activity(out: &mut Built, app: &App, bot: &Bot, entries: &[&Entry], width: usize, details: bool) {
    let t = theme();
    gap(out);
    let members: Vec<_> = if bot.group { app.working_group_members(bot).collect() } else { vec![] };
    if members.is_empty() {
        out.lines.push(Line::from(Span::styled(format!(" {}", bot.name), name_style(&bot.color))));
    }
    let steps = working_steps(entries);
    let open = details
        && app.steps_open.as_ref().is_some_and(|(id, turn)| id == &bot.id && Some(*turn) == running_turn(entries));
    // 成员标签按终端列宽排列，长标签自然换行。
    let mut row = Line::default();
    for member in &members {
        let label = Line::from(activity_label(app, member, width, true));
        if !row.spans.is_empty() {
            if row.width() + label.width() + 2 > width {
                out.lines.push(std::mem::take(&mut row));
            } else {
                row.spans.push(Span::raw("  "));
            }
        }
        row.spans.extend(label.spans);
    }
    if members.is_empty() {
        row = Line::from(activity_label(app, bot, width, false));
    }
    if details {
        // 终端详情按群回合展开，入口移到成员标签下方的群提示行。
        let line = out.lines.len() + usize::from(!members.is_empty());
        out.buttons.push((line, 0, u(width), Click::Steps));
        if members.is_empty() {
            row.spans.push(Span::styled(if open { "  v" } else { "  >" }, t.dim));
        }
    }
    out.lines.push(row);
    let scope = if members.is_empty() { String::new() } else { format!("{} · ", truncate(&bot.name, width / 3)) };
    let disclosure = if !details {
        ""
    } else if open {
        " · > hide"
    } else {
        " · > show"
    };
    let hint = format!("   {scope}{} steps so far{disclosure} · t full conversation", steps.len());
    out.lines.push(Line::from(Span::styled(hint, t.dim)));
    if open {
        // Claude's thinking text arrives as its thinking ends, so a turn can have no steps yet.
        if steps.is_empty() {
            out.lines.push(Line::from(Span::styled(format!("   {}", tr("Nothing yet.")), t.dim)));
        }
        let mut lines = vec![];
        for e in steps {
            push_step(&mut lines, app, e, width, false);
        }
        let earlier = lines.len().saturating_sub(STEP_LINES);
        if earlier > 0 {
            out.lines.push(Line::from(Span::styled(format!("   … {earlier} earlier lines · t shows them"), t.dim)));
        }
        out.lines.extend(lines.into_iter().skip(earlier));
    }
}

/// 生成一行活动标签，为状态和用时预留空间，长名称优先截断。
fn activity_label(app: &App, bot: &Bot, width: usize, member: bool) -> Vec<Span<'static>> {
    let t = theme();
    let text = if bot.activity.is_empty() { tr("Working…") } else { tr(&bot.activity) };
    if !member {
        return vec![
            Span::styled(format!(" {} ", spin(app)), t.secondary),
            Span::styled(truncate(text.as_ref(), width.saturating_sub(8)), t.secondary),
        ];
    }
    let timer = elapsed(bot.started_at);
    let room = width.saturating_sub(11 + timer.len());
    let name = truncate(&bot.name, room / 2);
    let text = truncate(text.as_ref(), room.saturating_sub(super::super::md::width(&name)));
    let (mark, style) = if bot.status == Status::NeedsInput { ("◆", t.amber) } else { (spin(app), t.secondary) };
    vec![
        Span::styled(format!(" {mark} "), style),
        Span::styled(format!("{name}: "), name_style(&bot.color)),
        Span::styled(text, style),
        Span::styled(format!(" {timer}"), t.dim),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{
        app::{Kind, Msg},
        net::Client,
    };
    use serde_json::json;

    fn render(app: &App, entries: &[Entry]) -> (String, usize) {
        render_with(app, entries, true)
    }

    fn render_with(app: &App, entries: &[Entry], details: bool) -> (String, usize) {
        let mut out = Built { lines: vec![], buttons: vec![], messages: vec![] };
        activity(&mut out, app, &app.bots["bot"], &entries.iter().collect::<Vec<_>>(), 80, details);
        (out.lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"), out.buttons.len())
    }

    /// 同时显示在本群思考和等审批的成员，排除仍在私聊工作的成员。
    #[test]
    fn group_activity_shows_each_working_member_in_roster_order() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Client::new("http://127.0.0.1:1", None), tx, String::new());
        app.online = true;
        for bot in [
            json!({"id": "bot", "name": "Team", "kind": "group", "members": ["a", "b", "c"]}),
            json!({"id": "a", "name": "Ada", "status": "working", "workingChat": "bot", "activity": "Thinking…"}),
            json!({"id": "b", "name": "Ben", "status": "needsInput", "workingChat": "bot", "activity": "Approval"}),
            json!({"id": "c", "name": "Cal", "status": "working", "workingChat": "c", "activity": "Thinking…"}),
        ] {
            app.on_msg(Msg::Event(json!({"type": "bot", "bot": bot})));
        }
        assert!(app.shows_activity(&app.bots["bot"]));
        let (text, buttons) = render(&app, &[]);
        let members = text.lines().next().unwrap();
        assert!(members.find("Ada:").unwrap() < members.find("Ben:").unwrap());
        assert!(!text.contains("Cal"));
        assert!(text.lines().nth(1).unwrap().contains("Team · 0 steps so far · > show"));
        assert_eq!(buttons, 1);

        let mut out = Built { lines: vec![], buttons: vec![], messages: vec![] };
        activity(&mut out, &app, &app.bots["bot"], &[], 25, false);
        assert!(out.lines[0].to_string().contains("Ada:"));
        assert!(out.lines[1].to_string().contains("Ben:"));
    }

    #[test]
    fn without_thinking_details_the_line_does_not_unfold() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Client::new("http://127.0.0.1:1", None), tx, String::new());
        app.on_msg(Msg::Event(json!({"type": "bot", "bot": {"id": "bot", "name": "Bot", "activity": "Terminal"}})));
        let entries = [Entry {
            id: "tool".into(),
            seq: 1,
            turn: 1,
            kind: Kind::Tool,
            data: json!({"title": "rg TODO", "bookkeeping": false}),
            created_at: 0,
        }];
        app.steps_open = Some(("bot".into(), 1));
        let (text, buttons) = render_with(&app, &entries, false);
        assert!(text.contains("1 steps so far · t full conversation"));
        assert!(!text.contains("> show") && !text.contains("> hide") && !text.contains("rg TODO"));
        assert_eq!(buttons, 0);
    }

    #[test]
    fn steps_unfold_under_the_activity_for_the_running_turn() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Client::new("http://127.0.0.1:1", None), tx, String::new());
        app.on_msg(Msg::Event(json!({"type": "bot", "bot": {
            "id": "bot", "name": "Bot", "activity": "Searching files…"
        }})));
        let entries = [
            Entry { id: "old".into(), seq: 1, turn: 1, kind: Kind::Tool, data: json!({}), created_at: 0 },
            Entry {
                id: "tool".into(),
                seq: 2,
                turn: 2,
                kind: Kind::Tool,
                data: json!({"title": "rg TODO", "bookkeeping": false}),
                created_at: 0,
            },
            Entry {
                id: "thought".into(),
                seq: 3,
                turn: 2,
                kind: Kind::Thought,
                data: json!({"text": "Looking for the parser"}),
                created_at: 0,
            },
            Entry {
                id: "queued".into(),
                seq: 4,
                turn: 3,
                kind: Kind::User,
                data: json!({"status": "queued"}),
                created_at: 0,
            },
        ];
        let (folded, buttons) = render(&app, &entries);
        assert!(folded.contains("Searching files…"));
        assert!(folded.contains("2 steps so far · > show · t full conversation"));
        assert!(!folded.contains("Looking for the parser") && !folded.contains("rg TODO"));
        assert_eq!(buttons, 1);

        app.steps_open = Some(("bot".into(), 2));
        let (open, _) = render(&app, &entries);
        assert!(open.contains("> hide"));
        assert!(open.contains("rg TODO") && open.contains("Looking for the parser"));

        // A new turn starts folded.
        app.steps_open = Some(("bot".into(), 1));
        assert!(!render(&app, &entries).0.contains("rg TODO"));
    }

    #[test]
    fn a_turn_without_steps_unfolds_to_nothing_yet() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Client::new("http://127.0.0.1:1", None), tx, String::new());
        app.on_msg(Msg::Event(json!({"type": "bot", "bot": {"id": "bot", "name": "Bot", "activity": "Thinking…"}})));
        let entries =
            [Entry { id: "u".into(), seq: 1, turn: 1, kind: Kind::User, data: json!({"text": "hi"}), created_at: 0 }];
        let (folded, buttons) = render(&app, &entries);
        assert!(folded.contains("Thinking…  >") && folded.contains("> show") && !folded.contains("Nothing yet."));
        assert_eq!(buttons, 1);
        app.steps_open = Some(("bot".into(), 1));
        assert!(render(&app, &entries).0.contains("Nothing yet."));
    }
}
