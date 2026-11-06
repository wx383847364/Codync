//! 投递 turn 的执行和结果保存。房间等待超时不丢弃在途结果。

use super::{Actor, Done};
use crate::store::Lane;
use crate::store::deliveries::{Delivery, Status};
use anyhow::{Result, ensure};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

pub(super) struct ActiveDelivery {
    pub delivery: Delivery,
    pub output: Option<String>,
    pub output_open: bool,
    /// 当前 prompt 实际展示的消息序号上界，与投递排序序号分开。
    pub shown_seq: i64,
    pub reply: oneshot::Sender<bool>,
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
        let claimed = self.hub.claim_delivery(&id, &self.cfg.id);
        match claimed {
            Ok(Some(delivery)) => {
                self.active_delivery = Some(ActiveDelivery {
                    delivery: delivery.clone(),
                    reply,
                    output: None,
                    output_open: false,
                    shown_seq: 0,
                });
                let prepared = crate::chat::group::delivery_prompt(&self.hub, &delivery, &self.cfg);
                match prepared {
                    Ok((prompt, shown)) => {
                        if let Some(active) = self.active_delivery.as_mut() {
                            active.shown_seq = shown;
                        }
                        if let Err(error) =
                            self.start_turn(Lane::main(&delivery.room_id), &[], &prompt, false, done_tx).await
                        {
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
            self.session_id = None;
            self.relay.recap = None;
            let _ = self.dreamer.send(crate::chat::memory::DreamEvent::Flush);
            crate::chat::group::wake(&self.hub, Some(&self.cfg.id));
        }
    }
}
