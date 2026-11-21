//! The user's queued messages before their turn comes: handled now (`steer`) or taken back (`unqueue`).

use super::{Actor, Cmd, Done, Queued};
use crate::agent::acp::Acp;
use crate::analytics::{self, Steer, TurnSource};
use crate::chat::team::RequestStatus;
use crate::chat::uploads;
use crate::hub::BotStatus;
use crate::store::{Lane, deliveries::Guidance};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// How long the agent may take to say whether a steered message joined its turn.
const STEER_ANSWER: Duration = Duration::from_secs(15);

/// 等待同一个 ACP 引导回执的私聊消息或群投递。
pub(super) enum InFlight {
    User { taken_back: bool },
    Group { source_id: String, prompt: String },
}

/// What the agent's answer to a steer made of the message.
fn steer_outcome(answer: &Result<Value>) -> Steer {
    match answer {
        Ok(v) if v["outcome"] == "injected" => Steer::Joined,
        Ok(v) if v["outcome"] == "promptRequired" => Steer::Next,
        _ => Steer::Failed,
    }
}

impl Actor {
    /// Where the user's queued message `entry_id` waits.
    fn queued_at(&self, entry_id: &str) -> Option<usize> {
        self.queue.iter().position(|q| matches!(q, Queued::User { entry_id: id, .. } if id == entry_id))
    }

    /// The running turn answers the user's own messages in `lane`: not a routine's, a room's, another bot's or the
    /// host's, nor a compaction, nor one the user already stopped.
    fn answers_user_in(&self, lane: &Lane) -> bool {
        self.turn.is_some()
            && !self.stop_requested
            && self.lane == *lane
            && self.compacting.is_none()
            && self.source() == TurnSource::User
    }

    /// 私聊引导优先注入本回合，适配器不支持时沿用原来的中断或提到队首。
    pub(super) async fn steer(&mut self, entry_id: String) {
        // Already with the agent.
        if self.steers_in_flight.contains_key(&entry_id) {
            return;
        }
        let Some(at) = self.queued_at(&entry_id) else { return };
        let Some(Queued::User { lane, text, .. }) = self.queue.get(at) else { return };
        let own = self.answers_user_in(lane);
        if own && let Some(session) = self.steering_session() {
            let text = text.clone();
            self.close_seg();
            self.hand_to_agent(entry_id, &text, InFlight::User { taken_back: false }, session);
            return;
        }
        self.move_first(at);
        let how = if own {
            self.cancel_turn().await;
            Steer::Interrupted
        } else {
            Steer::MovedUp
        };
        analytics::message_steered(&self.hub, &self.cfg.backend, how, false);
    }

    /// 只引导点击时的本群回合；没有可接收的回合时，原排队记录保持有效。
    pub(super) fn guide_delivery(&mut self, id: String, active_id: &str) -> Result<()> {
        if matches!(self.steers_in_flight.get(&id), Some(InFlight::Group { .. })) {
            return Ok(());
        }
        let Some(active) = self.active_delivery.as_ref().filter(|d| d.delivery.id == active_id) else {
            anyhow::bail!("成员的当前回合已结束或已切换");
        };
        let state = self.hub.runtime(&self.cfg.id);
        ensure!(
            !self.stop_requested
                && self.held_done.is_none()
                && state.status == BotStatus::Working
                && state.guiding_delivery_id.as_deref() == Some(active_id),
            "成员当前无法接收引导，消息仍在排队"
        );
        // ACP 断开可能先于回合结束消息到达，持久占用前必须拿到可用会话。
        let session = self.steering_session().ok_or_else(|| anyhow!("成员当前无法接收引导，消息仍在排队"))?;
        let room = self
            .hub
            .store
            .bot(&active.delivery.room_id)?
            .filter(|row| !row.deleted)
            .ok_or_else(|| anyhow!("群聊已删除"))?;
        let source = self.hub.start_guidance(&id, active_id, &self.cfg.id)?;
        let text = crate::chat::prompt::of_entry_in(&self.hub, &self.cfg, &room.config, &source.data);
        let prompt = format!("User guidance for the current task in this group:\n{text}");
        if let Some(active) = self.active_delivery.as_mut() {
            // 先登记未知结果，断开或退出时也不会将可能已经送达的消息再执行一次。
            active.guidance.push((id.clone(), Guidance::Uncertain));
        }
        // 引导的回执可能晚于最后一段输出，继续让原回合组装完整回复。
        self.hand_to_agent(id, &prompt, InFlight::Group { source_id: source.id, prompt: prompt.clone() }, session);
        Ok(())
    }

    /// 取得声明支持引导的 ACP 连接和当前会话。
    fn steering_session(&self) -> Option<(Arc<Acp>, String)> {
        let conn = self.conn.as_ref().filter(|conn| conn.sessions.steering)?;
        Some((conn.acp.clone(), self.turn_session.clone()?))
    }

    /// 发送引导并持有回合完成通知，直到 ACP 回执到达。
    fn hand_to_agent(&mut self, id: String, text: &str, in_flight: InFlight, (acp, sid): (Arc<Acp>, String)) {
        let (hub, bot) = (self.hub.clone(), self.cfg.id.clone());
        let params = json!({
            "sessionId": sid,
            "prompt": [{"type": "text", "text": text}],
            // A turn that just ended doesn't take it: it stays queued for the next one.
            "_meta": {"steering": {"idleBehavior": "promptRequired"}},
        });
        self.steers_in_flight.insert(id.clone(), in_flight);
        tokio::spawn(async move {
            let answer = tokio::time::timeout(STEER_ANSWER, acp.request("_session/steering", params)).await;
            let outcome = answer.unwrap_or_else(|_| Err(anyhow!("the agent didn't answer the steer")));
            // Refused only once the bot was deleted meanwhile: no one is left to tell.
            let _ = hub.send_cmd(&bot, Cmd::Steered { id, outcome });
        });
    }

    /// Moves what waits at `at` first in line; a compaction that is due still goes before it.
    fn move_first(&mut self, at: usize) {
        let Some(item) = self.queue.remove(at) else { return };
        let first = self.queue.iter().position(|q| !matches!(q, Queued::Compact { .. })).unwrap_or(self.queue.len());
        self.queue.insert(first, item);
        self.renumber_waiting();
    }

    /// Hands the waiting messages' turn numbers out again in their new order: a batch runs as its last message's turn
    /// (`start_turn`), and the clients take the highest for the running one.
    fn renumber_waiting(&self) {
        let waiting: Vec<_> = self
            .queue
            .iter()
            .filter_map(|q| match q {
                Queued::User { entry_id, .. } => self.hub.store.entry(entry_id),
                _ => None,
            })
            .collect();
        let mut turns: Vec<i64> = waiting.iter().map(|e| e.turn).collect();
        turns.sort_unstable();
        for (e, turn) in waiting.iter().zip(turns) {
            if e.turn != turn {
                self.hub.set_entry_turn(&e.id, Some(turn), &e.data);
            }
        }
    }

    /// 回合结束和引导回执独立到达，完成事务等全部回执，期间停止发布引导入口。
    pub(super) async fn turn_ended(&mut self, done: Done, done_tx: &mpsc::UnboundedSender<Done>) {
        self.hub.set_runtime(&self.cfg.id, |r| r.guiding_delivery_id = None);
        if self.steers_in_flight.is_empty() {
            self.on_turn_done(done, done_tx).await;
        } else {
            self.held_done = Some(done);
        }
    }

    /// 最后一条 ACP 回执到达后，提交此前等待的回合结果。
    pub(super) async fn steer_answered(
        &mut self,
        entry_id: &str,
        outcome: &Result<Value>,
        done_tx: &mpsc::UnboundedSender<Done>,
    ) {
        self.steered(entry_id, outcome);
        if self.steers_in_flight.is_empty()
            && let Some(done) = self.held_done.take()
        {
            self.on_turn_done(done, done_tx).await;
        }
    }

    /// ACP 回执决定消息加入当前回合、恢复排队或保留待核查状态。
    fn steered(&mut self, entry_id: &str, outcome: &Result<Value>) {
        let Some(steer) = self.steers_in_flight.remove(entry_id) else { return };
        if let Err(error) = outcome {
            tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't confirm steering");
        }
        let how = steer_outcome(outcome);
        let group = matches!(steer, InFlight::Group { .. });
        analytics::message_steered(&self.hub, &self.cfg.backend, how, group);
        let taken_back = match steer {
            InFlight::User { taken_back } => taken_back,
            InFlight::Group { source_id, prompt } => {
                self.guidance_answered(entry_id, source_id, &prompt, how);
                return;
            }
        };
        // A Stop meanwhile dropped it from the queue (and marked it cancelled).
        let Some(at) = self.queued_at(entry_id) else { return };
        if how != Steer::Joined {
            if taken_back {
                self.unqueue(entry_id);
            } else if how == Steer::Next {
                self.move_first(at);
            }
            return;
        }
        let Some(Queued::User { text, .. }) = self.queue.remove(at) else { return };
        // The host's copy, never `removed`, also brings it back to a client that took it back meanwhile.
        if let Some(mut e) = self.hub.store.entry(entry_id) {
            e.data["status"] = json!(RequestStatus::Sent);
            self.hub.set_entry_turn(entry_id, self.turn, &e.data);
        }
        self.joined = true;
        self.turn_entries.push(entry_id.to_owned());
        if let Some(turn_text) = &mut self.turn_text {
            turn_text.push_str("\n\n");
            turn_text.push_str(&text);
        }
        self.relay_joined(Some(entry_id), &text);
        // `send --wait` learns a started message's turn from the bot's next event, and a status and activity that stay
        // the same send none.
        self.hub.emit_bot(&self.cfg.id);
    }

    /// 将回执保留到完成事务；只有明确拒绝加入的消息才可以恢复排队。
    fn guidance_answered(&mut self, id: &str, source_id: String, prompt: &str, how: Steer) {
        let Some(active) = self.active_delivery.as_mut() else { return };
        if let Some((_, status)) = active.guidance.iter_mut().find(|(guide, _)| guide == id) {
            *status = match how {
                Steer::Joined => Guidance::Joined,
                Steer::Next => Guidance::Deferred,
                _ => Guidance::Uncertain,
            };
        }
        if how == Steer::Joined {
            active.shown_entries.push(source_id);
            self.joined = true;
            self.relay_joined(None, prompt);
        }
    }

    /// `unqueue`: the message never goes out, the chat no longer shows it, and its files leave the bot's folder.
    /// 老客户端的纯 /new 只在私聊容量提醒期间视为命令，重启恢复也沿用此识别。
    pub(super) fn consume_handoff_command(&self, lane: &Lane, text: &str, entry_id: &str) -> bool {
        if *lane != Lane::main(&self.cfg.id)
            || text != "/new"
            || !crate::chat::handoff::approve_from_session_switch(&self.hub, &self.cfg.id)
        {
            return false;
        }
        self.mark_user_messages(&[entry_id.to_owned()]);
        true
    }

    /// 仅更新用户消息，机器人间通知的终态由协作模块写入。
    pub(super) fn mark_user_messages(&self, ids: &[String]) {
        for id in ids {
            if let Some(mut entry) =
                self.hub.store.entry(id).filter(|entry| entry.kind == crate::store::EntryKind::User.as_str())
            {
                entry.data["status"] = json!(RequestStatus::Sent);
                self.hub.set_entry(id, &entry.data);
            }
        }
    }

    /// 取消未执行的消息，同时释放失去待执行任务的容量提醒。
    pub(super) fn unqueue(&mut self, entry_id: &str) {
        // Already with the agent, which may take it: `steered` decides.
        if let Some(InFlight::User { taken_back }) = self.steers_in_flight.get_mut(entry_id) {
            *taken_back = true;
            return;
        }
        let Some(at) = self.queued_at(entry_id) else { return };
        self.queue.remove(at);
        self.clear_empty_capacity_handoff();
        if let Some(mut e) = self.hub.store.entry(entry_id) {
            e.data["status"] = json!(RequestStatus::Cancelled);
            e.data["removed"] = true.into();
            self.hub.set_entry(entry_id, &e.data);
            self.delete_uploads(&e.data);
            // The roster previews the chat's newest message.
            self.hub.emit_bot(&self.cfg.id);
        }
    }

    /// Deletes the files of a message (its saved `data`) that never went out.
    fn delete_uploads(&self, data: &Value) {
        let ids: Vec<String> = data["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["id"].as_str().map(str::to_owned))
            .collect();
        if ids.is_empty() {
            return;
        }
        let (root, bot) = (uploads::root(&self.cfg), self.cfg.id.clone());
        tokio::task::spawn_blocking(move || {
            for id in ids {
                if let Err(error) = uploads::remove(&root, &id) {
                    tracing::warn!(%bot, error = format!("{error:#}"), "couldn't delete a removed message's file");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agents_answer_says_what_became_of_the_message() {
        assert_eq!(steer_outcome(&Ok(json!({"outcome": "injected"}))), Steer::Joined);
        assert_eq!(steer_outcome(&Ok(json!({"outcome": "promptRequired"}))), Steer::Next);
        assert_eq!(steer_outcome(&Ok(json!({"outcome": "somethingNew"}))), Steer::Failed);
        assert_eq!(steer_outcome(&Err(anyhow!("gone"))), Steer::Failed);
        assert_eq!(json!(Steer::MovedUp), "moved_up", "as the event reports it");
    }
}
