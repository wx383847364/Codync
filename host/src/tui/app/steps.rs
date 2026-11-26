//! The working line's steps: what the running turn has done so far, unfolded for one bot and turn.

use super::{After, App, Bot, Entry, Overlay};
use crate::store::deliveries::DeliveryControl;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

/// The turn running now; a queued message already holds the next turn.
pub(in crate::tui) fn running_turn(entries: &[&Entry]) -> Option<i64> {
    entries.iter().filter(|e| e.data["status"] != "queued").map(|e| e.turn).max()
}

/// The running turn's thinking, narration, tool calls and plan, in order.
pub(in crate::tui) fn working_steps<'a>(entries: &[&'a Entry]) -> Vec<&'a Entry> {
    let turn = running_turn(entries);
    entries.iter().copied().filter(|e| Some(e.turn) == turn && e.is_working_step()).collect()
}

impl App {
    /// 键盘入口可选择任意一条任务，保存快照，避免任务列表变化后误选其他行。
    pub(super) fn open_delivery_controls(&mut self) {
        let Some(bot) = self.bot().filter(|bot| self.online && bot.group) else { return };
        let room = bot.id.clone();
        let tasks = bot
            .delivery_controls
            .iter()
            .filter(|task| self.can_toggle_delivery(bot, task))
            .cloned()
            .collect::<Vec<_>>();
        if !tasks.is_empty() {
            self.overlays.push(Overlay::DeliveryControls { room, tasks, cursor: 0 });
        }
    }

    /// 上下键选择任务，回车只提交选中的回合。
    pub(super) fn delivery_control_key(
        &mut self,
        room: String,
        tasks: Vec<DeliveryControl>,
        mut cursor: usize,
        key: KeyEvent,
    ) -> Option<Overlay> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(tasks.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(task) = tasks.get(cursor) {
                    self.toggle_delivery(&room, task);
                }
                return None;
            }
            _ => {}
        }
        Some(Overlay::DeliveryControls { room, tasks, cursor })
    }

    /// 阶段关闭时继续入口等重新开放，离线和版本不匹配时禁用操作。
    pub(in crate::tui) fn can_toggle_delivery(&self, bot: &Bot, task: &DeliveryControl) -> bool {
        self.online
            && self.mismatch.is_none()
            && (task.executor.as_deref() != Some("host") || self.group_local_tasks)
            && (task.status == "started"
                || (task.status == "paused" && !bot.stage.as_ref().is_some_and(crate::store::StageGroup::read_only)))
    }

    /// 鼠标和键盘发送相同的身份与目标状态，Host 会复核过期请求。
    pub(super) fn toggle_delivery(&self, room: &str, task: &DeliveryControl) {
        if self.bots.get(room).is_some_and(|bot| self.can_toggle_delivery(bot, task)) {
            self.call(
                "setDeliveryPaused",
                json!({"botId":room,"deliveryId":task.delivery_id,
                "startedAt":task.started_at,"paused":task.status == "started"}),
                After::Nothing,
            );
        }
    }

    /// 按群成员顺序读取本群正在执行或等待审批的成员。
    pub(in crate::tui) fn working_group_members<'a>(&'a self, group: &'a Bot) -> impl Iterator<Item = &'a Bot> {
        group.members.iter().filter_map(|id| {
            // 成员可能正在私聊或其他群工作。
            let bot = self.bots.get(id)?;
            // 从快照过滤已暂停、但仍在处理取消命令的旧活动。
            let controls = group.delivery_controls.iter().filter(|task| task.target_bot_id == *id);
            let held = controls
                .clone()
                .any(|task| task.status != "started" && bot.started_at.unwrap_or(i64::MAX) <= task.started_at)
                && !controls.clone().any(|task| task.status == "started");
            (bot.busy() && bot.working_chat.as_deref() == Some(&group.id) && !held).then_some(bot)
        })
    }

    /// 仅在线时呈现实时活动，成员事件可以先于群组汇总到达。
    pub(in crate::tui) fn shows_activity(&self, bot: &Bot) -> bool {
        self.online
            && ((bot.busy() && bot.works_here() && (!bot.group || bot.working_chat.is_none()))
                || (bot.group && self.working_group_members(bot).next().is_some()))
    }

    /// `>` or a click on the working line: unfolds or folds the selected bot's steps for this turn, once thinking
    /// details are on.
    pub(super) fn toggle_steps(&mut self) {
        let Some(bot) = self.bot().filter(|b| self.shows_activity(b)) else { return };
        let Some(turn) = running_turn(&self.lane(&bot.id)) else { return };
        if !super::super::details::enabled() {
            self.flash("Turn on Show thinking details (^k) to unfold the steps");
            return;
        }
        let key = (bot.id.clone(), turn);
        self.steps_open = if self.steps_open.as_ref() == Some(&key) { None } else { Some(key) };
    }
}

#[cfg(test)]
mod tests {
    use super::super::Kind;
    use super::*;
    use serde_json::json;

    fn entry(id: &str, turn: i64, kind: Kind, data: serde_json::Value) -> Entry {
        Entry { id: id.into(), seq: 0, turn, kind, data, created_at: 0 }
    }

    #[test]
    fn steps_are_the_running_turns_work_without_what_the_chat_or_memory_show() {
        let entries = [
            entry("old", 1, Kind::Tool, json!({})),
            entry("u2", 2, Kind::User, json!({"text": "go"})),
            entry("thinking", 2, Kind::Thought, json!({"text": "Checking the build"})),
            entry("blank", 2, Kind::Thought, json!({"text": "  "})),
            entry("says", 2, Kind::Agent, json!({"text": "Running the tests"})),
            entry("terminal", 2, Kind::Tool, json!({"title": "npm test", "bookkeeping": false})),
            entry("memory", 2, Kind::Tool, json!({"title": "mcp__memory__edit_memory", "bookkeeping": true})),
            entry("unflagged", 2, Kind::Tool, json!({"title": "an older host's call"})),
            entry("plan", 2, Kind::Plan, json!({"entries": []})),
            entry("reply", 2, Kind::Agent, json!({"text": "Done", "final": true})),
            entry("card", 2, Kind::Permission, json!({})),
            entry("pass", 2, Kind::Agent, json!({"text": "(pass)"})),
            entry("q", 3, Kind::User, json!({"status": "queued"})),
        ];
        let ids: Vec<_> = working_steps(&entries.iter().collect::<Vec<_>>()).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["thinking", "says", "terminal", "plan"]);
    }
}
