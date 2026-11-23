//! 群派工在安全边界确认换会话，原始任务仍由同一个 runner 领取。

use super::{Actor, Adapter, group_context::fits_context};
use crate::store::deliveries::member_session::MemberSessionRequest;
use crate::store::deliveries::{
    Delivery,
    handoff::{GroupHandoff, Phase},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};

impl Actor {
    /// 仅会话历史挤占窗口时提供换会话，摘要也必须在估算的余量内。
    pub(super) fn draft_delivery_handoff(
        &self,
        d: &Delivery,
        sid: &str,
        slot: Option<&str>,
        window: u64,
        incoming: u64,
    ) -> Result<bool> {
        if !fits_context(0, window, incoming) {
            return Ok(false);
        }
        let mut text = if slot.is_none() {
            crate::chat::handoff::build(&self.hub.store, &self.cfg.id, sid, None)?
        } else {
            let snapshot = self.hub.store.group_context(&d.id, &self.cfg.id)?;
            let last = snapshot
                .recent
                .iter()
                .rev()
                .find(|entry| entry.kind == "agent" && entry.data["author"] == self.cfg.id)
                .and_then(|entry| entry.data["text"].as_str())
                .unwrap_or_default();
            let plan = snapshot
                .plans
                .iter()
                .rev()
                .find(|entry| entry.data["author"] == self.cfg.id)
                .map(|entry| crate::chat::handoff::plan_text(&entry.data))
                .unwrap_or_default();
            let task = d
                .body
                .as_deref()
                .or_else(|| snapshot.source.as_ref()?.data["text"].as_str())
                .or_else(|| snapshot.root.as_ref()?.data["text"].as_str())
                .unwrap_or_default();
            format!(
                "当前群派工：{}\n上一条群回复：{}\n计划：{}",
                task.chars().take(600).collect::<String>(),
                last.chars().take(650).collect::<String>(),
                plan.chars().take(450).collect::<String>()
            )
        };
        // 空间很紧时先给出短交接，当前任务的完整材料仍由原派工提供。
        text = text.chars().take(6_000).collect();
        if slot.is_some()
            && !fits_context(0, window, incoming.saturating_add(codync_mem::tokens::estimate(&text) as u64))
        {
            text = "接续当前群派工。完整任务、用户约束和待办随原派工提供。".into();
        }
        if slot.is_some()
            && !fits_context(0, window, incoming.saturating_add(codync_mem::tokens::estimate(&text) as u64))
        {
            return Ok(false);
        }
        let draft = GroupHandoff {
            phase: Phase::Draft,
            revision: uuid::Uuid::new_v4().to_string(),
            session: sid.into(),
            generation: self.hub.store.scoped_context_generation(&self.cfg.id, &self.cfg.backend, slot, sid)?,
            detached: false,
            replacement: None,
            replacement_generation: None,
            backend: self.cfg.backend.clone(),
            model: self.cfg.model.clone(),
            slot: slot.map(str::to_owned),
            text,
            window,
            incoming,
        };
        self.hub.group_handoff_update(|| Ok(((), self.hub.store.draft_group_handoff(&d.id, &draft)?)))?;
        self.hub.emit_bot(&self.cfg.id);
        crate::analytics::capture(
            &self.hub,
            crate::analytics::Event::GroupContextPrepared,
            json!({"outcome":"waiting_handoff"}).as_object().cloned().unwrap_or_default(),
        );
        Ok(true)
    }

    /// 领取前恢复确认或重启中的交接；只清理这个派工对应的活跃槽。
    pub(super) fn delivery_handoff(
        &mut self,
        d: &Delivery,
        slot: Option<&str>,
        incoming: Option<u64>,
    ) -> Result<Option<GroupHandoff>> {
        let Some(before) = self.hub.store.group_handoff(&d.id)? else { return Ok(None) };
        if matches!(before.phase, Phase::Sent | Phase::Superseded) {
            return Ok(None);
        }
        let handoff = self.hub.group_handoff_update(|| {
            self.hub.store.switch_group_handoff(&d.id, &self.cfg.backend, self.cfg.model.as_deref(), slot, incoming)
        })?;
        if let Some(handoff) = &handoff {
            if handoff.phase == Phase::Switched && !before.detached {
                if slot.is_some() {
                    self.run_sessions.retain(|slot, _| crate::store::slotted_group(slot) != Some(&d.room_id));
                } else {
                    self.session_id = None;
                    self.relay.recap = None;
                    self.main_session_changed();
                }
            }
            if handoff.phase == Phase::Superseded {
                self.hub.emit_bot(&self.cfg.id);
                self.context_blocks.remove(&d.id);
            }
        }
        Ok(handoff)
    }

    /// 优先调用已声明的归档能力，明确失败时才尝试已声明的删除能力。
    pub(super) async fn retire_delivery_session(&mut self, new_session: &str) {
        // 新会话确实建立后才整理执行器的旧记录，失败时仍可访问本机归档。
        let Some(active) = self.active_delivery.as_ref() else { return };
        let Ok(Some(handoff)) = self.hub.store.group_handoff(&active.delivery.id) else { return };
        if handoff.phase != Phase::Switched || handoff.backend != self.cfg.backend || handoff.session == new_session {
            return;
        }
        let opened_on = active.archive_connection;
        self.retire_context_session(&handoff, opened_on).await;
    }

    /// 空闲归档和原派工使用同样的执行器能力及会话归属检查。
    async fn retire_context_session(&mut self, handoff: &GroupHandoff, opened_on: Option<std::time::Instant>) {
        let sid = handoff.session.as_str();
        let Some(conn) = self.conn.as_ref().filter(|conn| conn.backend == handoff.backend) else { return };
        // 执行器重启后进程内 ID 可能复用。若不能证明进程和槽都匹配，只按 Codex 的持久线程 UUID 寻址。
        let owner = conn.session_slots.get(sid);
        let same_process =
            opened_on == Some(conn.started) && owner.is_some_and(|owner| owner.as_deref() == handoff.slot.as_deref());
        let known_persistent = conn.adapter == Adapter::Codex && uuid::Uuid::parse_str(sid).is_ok() && owner.is_none();
        if !same_process && !known_persistent {
            return;
        }
        let acp = conn.acp.clone();
        let methods = [conn.sessions.archive, conn.sessions.delete.then_some("session/delete")];
        for method in methods.into_iter().flatten() {
            match tokio::time::timeout(std::time::Duration::from_secs(3), acp.request(method, json!({"sessionId":sid})))
                .await
            {
                Ok(Ok(_)) => return,
                Ok(Err(error)) => {
                    tracing::warn!(%error, method, "couldn't retire the archived group session");
                    if error.downcast_ref::<crate::agent::acp::RpcError>().is_none() {
                        break;
                    }
                }
                Err(_) => break, // 响应未知时保留本机归档，避免接着删除一个可能已归档的会话。
            }
        }
        if same_process {
            self.close_reopenable(sid).await;
        }
    }

    /// 成员卡只负责进入已有派工交接或准备该范围的主动摘要。
    pub(super) fn prepare_member_session(&self, room: &str) -> Result<Value> {
        let (current, blocked) = self.hub.store.group_member_context(room, &self.cfg.id)?;
        if let Some(shorter) = self.hub.store.review_shorter_member_summary(&self.cfg.id, room)? {
            return Ok(shorter);
        }
        if let Some(handoff) = blocked {
            return Ok(handoff);
        }
        ensure!(current.recommend(), "This session is below the recommended threshold.");
        // 显示推荐不要求执行器上报窗口，但独立群交接预算不能把未知值当零后归档旧会话。
        ensure!(
            current.shared || current.size.is_some(),
            "The actual model window is unavailable. Wait for context usage to refresh."
        );
        let session = current.session.ok_or_else(|| anyhow!("No active session"))?;
        let text = if current.shared {
            crate::chat::handoff::build(&self.hub.store, &self.cfg.id, &session, None)?
        } else {
            // 群摘要只读取自己可见的消息，不使用私聊或其他群的记录。
            let entries = self.hub.store.history(room, i64::MAX, 24)?;
            let lines = entries
                .iter()
                .rev()
                .filter(|e| e.kind == "user" || e.kind == "agent" && e.data["author"] == self.cfg.id)
                .filter_map(|e| e.data["text"].as_str())
                .take(5)
                .collect::<Vec<_>>();
            let mut summary = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
            if summary.trim().is_empty() {
                summary = "Continue the work in this group using its latest requests and results.".into();
            }
            summary
        };
        let request = MemberSessionRequest {
            room: room.to_owned(),
            retirement_pending: false,
            needs_shorter_summary: false,
            generation: current.generation.ok_or_else(|| anyhow!("Wait for context usage to refresh"))?,
            handoff: GroupHandoff {
                phase: Phase::Draft,
                revision: uuid::Uuid::new_v4().to_string(),
                session,
                generation: None,
                detached: false,
                replacement: None,
                replacement_generation: None,
                backend: current.backend,
                model: current.model,
                slot: current.slot,
                text: text.chars().take(6_000).collect(),
                window: current.size.unwrap_or_default(),
                incoming: 0,
            },
        };
        self.hub.group_handoff_update(|| self.hub.store.prepare_member_session(&self.cfg.id, &request))
    }

    /// 保存待执行的请求，让它优先于下一条群派工及私聊任务。
    pub(super) async fn apply_member_sessions(&mut self) -> Result<()> {
        if self.turn.is_some() || self.pending_completion.is_some() || self.routine_completion.is_some() {
            return Ok(());
        }
        for r in self.hub.store.queued_member_sessions(&self.cfg.id)? {
            if r.handoff.phase != Phase::Approved {
                if self.conn.is_some() && self.cfg.backend == r.handoff.backend {
                    self.retire_context_session(&r.handoff, None).await;
                    self.hub.store.finish_member_retirement(&self.cfg.id, &r)?;
                }
                continue;
            }
            let opened_on = self
                .conn
                .as_ref()
                .filter(|conn| conn.loaded.contains_key(&r.handoff.session) && self.cfg.backend == r.handoff.backend)
                .map(|conn| conn.started);
            let applied = self.hub.group_handoff_update(|| self.hub.store.apply_member_session(&self.cfg.id, &r))?;
            if applied {
                if r.handoff.slot.is_some() {
                    self.run_sessions.retain(|slot, _| crate::store::slotted_group(slot) != Some(&r.room));
                } else {
                    self.session_id = None;
                    self.relay.recap = None;
                    self.main_session_changed();
                }
                if self.conn.is_some() && self.cfg.backend == r.handoff.backend {
                    self.retire_context_session(&r.handoff, opened_on).await;
                    self.hub.store.finish_member_retirement(&self.cfg.id, &r)?;
                }
            }
            self.hub.emit_bot(&self.cfg.id);
        }
        Ok(())
    }

    /// 只选择一次主动摘要，在领取和实际发送前使用同一份业务提示。
    pub(super) async fn add_member_summary(&self, d: &Delivery, prompt: &mut String) -> Result<(bool, Option<String>)> {
        let Some(digest) = self.hub.store.member_group_summary(&self.cfg.id, &d.room_id)? else {
            return Ok((true, None));
        };
        let proposed = format!("<session-handoff>\n{}\n</session-handoff>\n\n{prompt}", digest.text);
        let known_window =
            self.hub.store.group_member_context(&d.room_id, &self.cfg.id)?.0.size.unwrap_or(digest.window);
        let size = self.fresh_group_estimate(&proposed).await? as u64;
        if !fits_context(0, known_window, size) {
            let detail =
                "The assignment and handoff do not fit the new session. Shorten the handoff on the member card.";
            self.hub.store.request_shorter_member_summary(&self.cfg.id, &d.room_id, &digest.revision)?;
            if d.last_error.as_deref() != Some(detail) {
                self.hub.delivery_context_problem(&d.id, &self.cfg.id, detail)?;
            }
            self.hub.emit_bot(&self.cfg.id);
            return Ok((false, None));
        }
        *prompt = proposed;
        Ok((true, Some(digest.revision)))
    }

    /// 执行器接到派工后更新原卡，界面可以折叠为已接续状态。
    pub(super) fn delivery_handoff_sent(&self, id: &str) {
        if let Err(error) = self.hub.group_handoff_update(|| Ok(((), self.hub.store.group_handoff_sent(id)?))) {
            tracing::warn!(%error, "couldn't update the resumed group delivery");
        }
    }
}
