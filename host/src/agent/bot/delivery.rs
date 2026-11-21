//! 投递 turn 的执行和结果保存。房间等待超时不丢弃在途结果。

use super::{Actor, Done, SegKind};
use crate::store::deliveries::{Delivery, Guidance, Status, handoff::Phase};
use crate::store::{Lane, group_session_slot};
use anyhow::{Result, ensure};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

pub(super) struct ActiveDelivery {
    pub delivery: Delivery,
    pub isolated: bool,
    /// 只在原连接和会话槽同时匹配时操作进程内旧会话。
    pub archive_connection: Option<Instant>,
    pub member_summary_revision: Option<String>,
    output: Option<String>,
    pub output_open: bool,
    /// 最近一次工具之后、被思考片段分开的正文，供尾部 pass 时保留。
    reply_before_pass: Option<String>,
    /// 本段输出期间工具是否继续执行，避免把进度当成结果。
    output_crossed_work: bool,
    /// 仅保留尚未结束的工具。是否继续工作不能依赖工具记录落库成功。
    pending_tools: HashMap<String, String>,
    /// 当前 prompt 确实展示的消息 ID；序号上界不能代表全部历史已读。
    pub shown_entries: Vec<String>,
    /// 引导来源的独立终态和原投递在同一事务提交。
    pub guidance: Vec<(String, Guidance)>,
    pub reply: oneshot::Sender<bool>,
}

impl ActiveDelivery {
    /// 为新的模型尝试清空输出，避免失败模型的片段成为下一次尝试的结果。
    pub(super) fn reset_output(&mut self) {
        self.output = None;
        self.output_open = false;
        self.output_crossed_work = false;
        self.reply_before_pass = None;
        self.pending_tools.clear();
    }

    /// 在内存中按片段缓冲输出，即使数据库写入失败也保留完整内容。
    pub(super) fn buffer_output(&mut self, kind: SegKind, text: &str) {
        if kind == SegKind::Text {
            if self.output_open {
                self.output.get_or_insert_default().push_str(text);
            } else {
                self.output = Some(text.to_owned());
                self.output_open = true;
                self.output_crossed_work = false;
            }
        } else if self.output_open
            && !self.output_crossed_work
            && self.output.as_deref().is_some_and(|reply| !crate::chat::group::is_pass(reply))
        {
            // 思考不开始新工作，后面如果只剩 pass，刚才的正文仍可交付。
            self.reply_before_pass.clone_from(&self.output);
        }
    }

    /// 工具继续工作时作废回退正文，同时保留正在拼接的流文本。
    pub(super) fn invalidate_reply_before_pass(&mut self) {
        self.reply_before_pass = None;
        self.output_crossed_work = true;
    }

    /// 最后一段只有 pass 时，回退到这次工作完成后已给出的正文。
    pub(super) fn reply_text(&self) -> Option<String> {
        let text = self.output.as_deref()?;
        if crate::chat::group::is_pass(text)
            && let Some(reply) = &self.reply_before_pass
        {
            return Some(reply.clone());
        }
        Some(text.to_owned())
    }

    /// 登记尚未结束的工具，以识别它后续真正的状态变化。
    pub(super) fn note_tool(&mut self, id: &str, status: &str) {
        if matches!(status, "pending" | "in_progress") {
            self.pending_tools.insert(id.to_owned(), status.to_owned());
        } else {
            self.pending_tools.remove(id);
        }
    }

    /// 返回工具是否继续执行或首次结束。重复、迟到和仅含内容的更新不会清除正文。
    pub(super) fn tool_advanced(&mut self, id: &str, status: &str) -> bool {
        let Some(previous) = self.pending_tools.get_mut(id) else { return false };
        match status {
            "in_progress" if previous == "pending" => status.clone_into(previous),
            "completed" | "failed" => {
                self.pending_tools.remove(id);
            }
            _ => return false,
        }
        true
    }
}

pub(super) struct PendingCompletion {
    pub active: ActiveDelivery,
    pub status: Status,
    pub text: Option<String>,
    pub detail: Option<String>,
    pub turn: i64,
    pub attempts: usize,
    pub next: Instant,
}

impl Actor {
    /// 以数据库状态识别用户暂停，继续请求可能已排队，但当前回合仍是旧执行。
    pub(super) fn delivery_held_after_pause(&self) -> bool {
        let Some(active) = &self.active_delivery else { return false };
        match self.hub.store.delivery(&active.delivery.id) {
            Ok(delivery) => delivery.is_some_and(|delivery| delivery.held_after_pause()),
            Err(error) => {
                tracing::warn!(%error, "could not check whether the active delivery was paused");
                false
            }
        }
    }

    /// 新工具或审批之后出现 pass 时，不能把此前的进度提升为结果。
    pub(super) fn close_seg_for_work(&mut self) {
        self.close_seg();
        if let Some(active) = self.active_delivery.as_mut() {
            active.invalidate_reply_before_pass();
        }
    }

    /// 只执行 runner 指定的 ID；忙时拒绝领取，原投递保持 queued。
    pub(super) async fn begin_delivery(
        &mut self,
        id: String,
        reply: oneshot::Sender<bool>,
        done_tx: &mpsc::UnboundedSender<Done>,
    ) {
        if self.pending_new_session.is_some()
            || self.turn.is_some()
            || !self.queue.is_empty()
            || self.pending_completion.is_some()
            || self.routine_completion.is_some()
        {
            let _ = reply.send(false);
            return;
        }
        let queued = match self.hub.store.delivery(&id) {
            Ok(Some(d)) if d.status == Status::Queued && d.target_bot_id == self.cfg.id => d,
            Ok(_) => {
                self.context_blocks.remove(&id);
                let _ = reply.send(false);
                return;
            }
            Err(error) => {
                tracing::error!(%error,"delivery preflight lookup failed");
                let _ = reply.send(false);
                return;
            }
        };
        let isolated = match self.has_group_session(&queued.room_id) {
            Ok(isolated) => isolated,
            Err(error) => {
                tracing::warn!(%error, "could not resolve the group session");
                let _ = reply.send(false);
                return;
            }
        };
        let slot = isolated.then(|| group_session_slot(&queued.room_id, &self.cfg.backend));
        let mut handoff = match self.delivery_handoff(&queued, slot.as_deref(), None) {
            Ok(Some(handoff)) if handoff.phase == Phase::Draft => {
                let _ = reply.send(false);
                return;
            }
            Ok(handoff) => handoff.filter(|handoff| matches!(handoff.phase, Phase::Approved | Phase::Switched)),
            Err(error) => {
                tracing::warn!(%error,"group handoff remains queued");
                let _ = reply.send(false);
                return;
            }
        };
        // 若该回合会切换后端，旧连接的用量不代表它将使用的会话。
        let session =
            if !isolated || self.next_turn_uses_current_backend() { self.session_for(slot.as_deref()) } else { None };
        // 维护先于领取；DB、会话和房间条件不变时不重复构建超窗任务包。
        match self.context_is_blocked(&queued, session.as_deref()).and_then(|blocked| {
            if handoff.is_some() {
                Ok(false)
            } else if blocked {
                Ok(true)
            } else {
                self.prepare_group_context(&queued, slot.as_deref(), session.as_deref(), None)
            }
        }) {
            Ok(true) => {
                let _ = reply.send(false);
                return;
            }
            Err(error) => {
                tracing::error!(%error,"context preflight failed; delivery kept queued");
                let _ = reply.send(false);
                return;
            }
            Ok(false) => {}
        }
        // 文件证据可能需要分片校验，构建提示包不阻塞 Tokio 执行线程。
        let (hub, config, work) = (self.hub.clone(), self.cfg.clone(), queued.clone());
        let mut prepared =
            tokio::task::spawn_blocking(move || crate::chat::group::delivery_prompt(&hub, &work, &config))
                .await
                .map_err(anyhow::Error::from)
                .and_then(std::convert::identity);
        let mut member_summary_revision = None;
        if let Ok(packet) = &mut prepared
            && isolated
            && handoff.is_none()
        {
            match self.add_member_summary(&queued, &mut packet.prompt).await {
                Ok((true, revision)) => member_summary_revision = revision,
                Ok((false, _)) => {
                    let _ = reply.send(false);
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "couldn't prepare member summary");
                    let _ = reply.send(false);
                    return;
                }
            }
        }
        let mut archive_connection = None;
        if let Ok(packet) = &prepared {
            if let Some(before) = &handoff {
                let opening = self.fresh_group_estimate(&packet.prompt).await;
                let current_connection = self.conn.as_ref().and_then(|conn| {
                    (conn.loaded.contains_key(&before.session)
                        && conn
                            .session_slots
                            .get(&before.session)
                            .is_some_and(|owner| owner.as_deref() == slot.as_deref()))
                    .then_some(conn.started)
                });
                match opening.and_then(|size| self.delivery_handoff(&queued, slot.as_deref(), Some(size as u64))) {
                    Ok(Some(after)) if after.phase == Phase::Switched => {
                        if before.phase == Phase::Approved {
                            archive_connection = current_connection;
                        }
                        handoff = Some(after);
                    }
                    Ok(_) => {
                        let _ = reply.send(false);
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "group handoff recheck failed; kept queued");
                        let _ = reply.send(false);
                        return;
                    }
                }
            } else {
                let estimated = self.prompt_estimate(&packet.prompt, session.as_deref(), slot.is_none()).await;
                let fresh = match &estimated {
                    Ok(estimate) if self.group_needs_fresh_estimate(session.as_deref(), *estimate) => {
                        match self.fresh_group_estimate(&packet.prompt).await {
                            Ok(fresh) => fresh,
                            Err(error) => {
                                tracing::warn!(%error, "fresh group context estimate failed; kept queued");
                                let _ = reply.send(false);
                                return;
                            }
                        }
                    }
                    _ => 0,
                };
                match estimated.and_then(|estimate| {
                    self.prepare_group_context(&queued, slot.as_deref(), session.as_deref(), Some((estimate, fresh)))
                }) {
                    Ok(true) => {
                        let _ = reply.send(false);
                        return;
                    }
                    Err(error) => {
                        tracing::error!(%error,"context space check failed; delivery kept queued");
                        let _ = reply.send(false);
                        return;
                    }
                    Ok(false) => {}
                }
            }
        }
        let claimed = self.hub.claim_delivery(&id, &self.cfg.id);
        match claimed {
            Ok(Some(delivery)) => {
                self.active_delivery = Some(ActiveDelivery {
                    delivery: delivery.clone(),
                    isolated,
                    archive_connection,
                    member_summary_revision,
                    reply,
                    output: None,
                    output_open: false,
                    reply_before_pass: None,
                    output_crossed_work: false,
                    pending_tools: HashMap::new(),
                    shown_entries: Vec::new(),
                    guidance: Vec::new(),
                });
                match prepared {
                    Ok(prepared) => {
                        self.context_blocks.remove(&id);
                        // 仅上报数量和估算来源；未知的真实窗口保持 null，不作为零计费量展示。
                        let (used, size) = self.reported_group_context(session.as_deref());
                        crate::analytics::capture(&self.hub,crate::analytics::Event::GroupContextPrepared,
                            serde_json::json!({"outcome":"prepared","protected_estimated_tokens":prepared.protected_estimate,"background_estimated_tokens":prepared.background_estimate,"background_budget":crate::chat::group::background_budget(),"context_used":used,"context_size":size}).as_object().cloned().unwrap_or_default());
                        tracing::debug!(
                            protected_estimate = prepared.protected_estimate,
                            background_estimate = prepared.background_estimate,
                            cutoff = prepared.cutoff,
                            "group context prepared"
                        );
                        if let Some(active) = self.active_delivery.as_mut() {
                            active.shown_entries = prepared.shown;
                        }
                        let prompt = if let Some(handoff) = &handoff.filter(|h| h.slot.is_some()) {
                            format!(
                                "<session-handoff>\nHandoff context for this assignment. Follow the original task below.\n{}\n</session-handoff>\n\n{}",
                                handoff.text, prepared.prompt
                            )
                        } else {
                            prepared.prompt
                        };
                        let result = self.start_turn(Lane::main(&delivery.room_id), &[], &prompt, false, done_tx).await;
                        if let Err(error) = result {
                            if self
                                .hub
                                .store
                                .delivery(&id)
                                .ok()
                                .flatten()
                                .is_some_and(|d| d.status == Status::Cancelled)
                            {
                                // 已持久取消且尚未发送的投递不是启动失败，不留下错误状态。
                                self.finish_delivery(Status::Cancelled, None, None);
                                self.turn = None;
                                self.hub.team.end_turn(&self.cfg.id);
                                self.hub.set_runtime(&self.cfg.id, |r| {
                                    // 取消后保存也可能失败，必须保留来源群恢复入口。
                                    r.status = if r.storage_error.is_some() {
                                        crate::hub::BotStatus::Error
                                    } else {
                                        crate::hub::BotStatus::Idle
                                    };
                                    r.activity = r.storage_error.clone().unwrap_or_default();
                                    if r.storage_error.is_none() {
                                        r.lane = None;
                                    }
                                    r.started_at = None;
                                });
                            } else {
                                self.start_error(error, done_tx).await;
                            }
                        }
                    }
                    Err(error) => self.finish_delivery(Status::Failed, None, Some(error.to_string())),
                }
            }
            Ok(None) => {
                let _ = reply.send(false);
            }
            Err(error) => {
                tracing::error!(%error,"delivery claim failed");
                let _ = reply.send(false);
            }
        }
    }

    /// 会话准备完成后提交保守发送边界；提交失败绝不发送 prompt。
    pub(super) fn commit_delivery_start(&self) -> Result<()> {
        if let Some(active) = &self.active_delivery {
            ensure!(self.hub.start_delivery(&active.delivery.id, &self.cfg.id)?, "delivery cancelled before prompt");
        }
        Ok(())
    }

    /// 结果先留内存，保存成功才释放等待位；失败只暂停当前 actor。
    pub(super) fn finish_delivery(&mut self, mut status: Status, text: Option<String>, mut detail: Option<String>) {
        let Some(active) = self.active_delivery.take() else { return };
        if self.pending_new_session == Some(true) {
            status = Status::Interrupted;
            detail = Some("因强制新建会话而中断".into());
        }
        if status == Status::NoReply && active.delivery.kind == crate::store::deliveries::Kind::Task {
            detail = Some("未提供结果".into());
        }
        self.pending_completion = Some(PendingCompletion {
            active,
            status,
            text,
            detail,
            turn: self.turn.unwrap_or(0),
            attempts: 0,
            next: Instant::now(),
        });
        self.retry_delivery_save(true);
        if let Some(pending) = self.pending_completion.as_mut() {
            pending.attempts = 0;
        }
    }

    /// 固定 3 秒、最多 5 次自动重试；手动重试不依赖卡片是否写出。
    pub(super) fn retry_delivery_save(&mut self, manual: bool) {
        let Some(pending) = self.pending_completion.as_mut() else { return };
        if !manual && (pending.attempts >= 5 || Instant::now() < pending.next) {
            return;
        }
        if manual {
            pending.attempts = 0;
        }
        pending.attempts += 1;
        pending.next = Instant::now() + Duration::from_secs(3);
        match self.hub.complete_delivery(
            &pending.active.delivery.id,
            pending.status,
            pending.text.as_deref(),
            pending.detail.as_deref(),
            pending.turn,
            &pending.active.guidance,
        ) {
            Ok(()) => {
                let pending = self.pending_completion.take().expect("completion retained until commit");
                self.hub.delivery_storage_error(&self.cfg.id, None);
                // 故障恢复发生在 turn 结束之后，单独刷新来源群，清除曾投影的错误状态。
                self.hub.emit_bot(&pending.active.delivery.room_id);
                let _ = pending.active.reply.send(true);
                crate::chat::group::wake(&self.hub, Some(&self.cfg.id));
            }
            Err(error) => {
                tracing::error!(bot=%self.cfg.id,%error,attempt=pending.attempts,"delivery result retained in memory");
                // 即使 prompt 准备前失败，仍可从持久身份恢复来源，供群视图重试保存。
                let lane = Lane::main(&pending.active.delivery.room_id);
                self.hub.set_runtime(&self.cfg.id, |state| state.lane = Some(lane));
                self.hub.delivery_storage_error(
                    &self.cfg.id,
                    Some("存储不可写：结果已保留，等待重试；若进程退出，未保存结果可能丢失，执行结果将不确定".into()),
                );
            }
        }
    }

    /// 默认等当前执行及结果保存结束后切换，强制切换也不能早于中断持久化。
    pub(super) fn apply_pending_session(&mut self) {
        if self.pending_new_session.is_some() && self.turn.is_none() && self.pending_completion.is_none() {
            if self.hub.runtime(&self.cfg.id).storage_error.is_some() {
                return;
            }
            if let Err(error) = self.hub.store.reset_main_session(&self.cfg.id) {
                tracing::error!(%error,"new session could not be saved");
                self.hub
                    .delivery_storage_error(&self.cfg.id, Some("存储不可写：旧会话已保留，等待重试保存后切换".into()));
                return;
            }
            self.pending_new_session = None;
            self.hub.set_runtime(&self.cfg.id, |r| r.pending_new_session = false);
            // 持久清理成功后才清除内存会话；queued投递随后在新会话执行。
            crate::chat::handoff::clear(&self.hub, &self.cfg.id);
            self.session_id = None;
            self.main_session_changed();
            self.relay.recap = None;
            let _ = self.dreamer.send(crate::chat::memory::DreamEvent::Flush);
            crate::chat::group::wake(&self.hub, Some(&self.cfg.id));
        }
    }
}
