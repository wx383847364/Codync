//! 群任务的领取前上下文维护；维护不承载业务结果，也不切换或遗忘共享会话。

use super::{Actor, Cmd, Conn, Done, Instructions, Queued};
use crate::chat::{context, memory};
use crate::hub::BotStatus;
use crate::store::{
    deliveries::{Delivery, Status},
    now_ms,
};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const CONTEXT_TARGET: u64 = 64_000;
const MAINTENANCE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(super) struct Maintenance {
    pub room: String,
    pub delivery: String,
    session: String,
    used: u64,
    epoch: u32,
    started: i64,
    pub confirmed: bool,
    deadline: Instant,
    timed_out: bool,
}

pub(super) struct Blocked {
    session: Option<String>,
    model: String,
    room_rev: i64,
    usage: Option<(u64, u64)>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Attempt {
    epoch: u32,
    used: u64,
    blocked: bool,
    confirmed: bool,
    started: i64,
}

/// 目标是上下文空间，不是协作轮数；已有 session 占用和回复余量都必须计算。
fn pressure(used: u64, size: u64, incoming: u64) -> (bool, bool) {
    if size == 0 {
        return (false, false);
    }
    let target = (size / 4 * 3).min(CONTEXT_TARGET);
    let reserve = (size / 16).clamp(2_048, 8_192);
    (used >= target, used.saturating_add(incoming).saturating_add(reserve) > size)
}

/// 使用量与窗口分别标记未知；整理后的旧占用不应被展示为当前统计。
fn reported_usage(usage: Option<(u64, u64)>, stale: bool) -> (Option<u64>, Option<u64>) {
    match usage {
        Some((used, size)) => ((!stale).then_some(used), (size != 0).then_some(size)),
        None => (None, None),
    }
}

impl Actor {
    /// 停止后的 ACP 仍可能退出中；已越过 MCP 桥的迟到命令也不得产生维护副作用。
    pub(super) fn maintenance_command(&self, cmd: Cmd) -> Option<Cmd> {
        if self.group_compaction.is_none() {
            return Some(cmd);
        }
        match cmd {
            Cmd::SendToUser { reply, .. } | Cmd::React { reply, .. } | Cmd::SendFile { reply, .. } => {
                let _ = reply.send(Err(anyhow!("tools are unavailable during context maintenance")));
            }
            Cmd::PostArtifact { reply, .. } => {
                let _ = reply.send(Err(anyhow!("tools are unavailable during context maintenance")));
            }
            Cmd::Approve { reply, .. } => {
                let _ = reply.send(false);
            }
            other => return Some(other),
        }
        None
    }

    /// 预检包含真正会补发的 profile、记忆和交接前缀；只读预览不消费提醒或会话水位。
    pub(super) async fn group_prompt_estimate(&self, text: &str) -> Result<usize> {
        let mut estimate = codync_mem::tokens::estimate(text);
        let Some(sid) = self.session_id.as_deref() else { return Ok(estimate) };
        let snapshot = self.snapshot(sid).await?;
        let channel = self.conn.as_ref().map_or(Instructions::FirstMessage, Conn::instructions);
        let resend = channel == Instructions::FirstMessage && self.profile_resend.contains(sid);
        if channel == Instructions::FirstMessage && (snapshot.unprompted || resend) {
            estimate = estimate.saturating_add(codync_mem::tokens::estimate(&snapshot.system));
        }
        // change_notice 会消费 put-back；预算只能调用同一材料的只读预览。
        if let Some((notice, _)) = memory::peek_change_notice(
            &self.hub.store,
            &self.cfg,
            sid,
            (snapshot.unprompted || resend).then_some(snapshot.memory.as_str()),
        ) {
            estimate = estimate.saturating_add(codync_mem::tokens::estimate(&notice));
        }
        if let Some((update, _)) = context::profile_update(&self.hub.store, &snapshot, &self.cfg) {
            estimate = estimate.saturating_add(codync_mem::tokens::estimate(&update));
        }
        for first in [self.relay.carried.as_deref(), self.handoff.as_deref()].into_iter().flatten() {
            estimate = estimate.saturating_add(codync_mem::tokens::estimate(first));
        }
        // profile 标签、重发说明和段落分隔一并预留，仍是估算而非实际 tokenizer。
        Ok(estimate.saturating_add(256))
    }

    /// 遥测不把缺失窗口报成零，也不把已整理但尚未重报的占用当作当前值。
    pub(super) fn reported_group_context(&self) -> (Option<u64>, Option<u64>) {
        let Some(sid) = self.session_id.as_deref() else { return (None, None) };
        let stale = self
            .context_attempts
            .get(sid)
            .is_some_and(|a| a.confirmed && self.context_usage_at.get(sid).copied().unwrap_or(0) <= a.started);
        reported_usage(self.context_usage(), stale)
    }

    /// 同一个会话和房间数据没有变化时只保持 queued，避免每三秒重复昂贵准备。
    pub(super) fn context_is_blocked(&self, d: &Delivery) -> Result<bool> {
        let Some(block) = self.context_blocks.get(&d.id) else { return Ok(false) };
        Ok(block.session == self.session_id
            && block.model == self.context_model()
            && block.room_rev == self.hub.store.group_context_revision(&d.room_id)?
            && block.usage == self.context_usage())
    }

    /// 来自真实 `usage_update` 的当前会话用量，未知不能冒充零。
    fn context_usage(&self) -> Option<(u64, u64)> {
        let sid = self.session_id.as_ref()?;
        self.context_usage_at.get(sid)?;
        self.conn.as_ref()?.usage.get(sid).copied()
    }

    /// 模型或后端变化会使旧的整理/阻塞指纹失效。
    fn context_model(&self) -> String {
        format!("{}:{:?}", self.cfg.backend, self.cfg.model)
    }

    /// 同一会话的尝试记录只防止无效整理循环，不限制可执行任务的累计次数。
    fn attempt_key(&self, sid: &str) -> String {
        format!("group.context.compact.{}.{}", self.cfg.id, sid)
    }

    /// 领取前安排一次同 sid 维护；不支持或无法释放空间时仍保留原投递与明确恢复路径。
    pub(super) fn prepare_group_context(&mut self, d: &Delivery, incoming: Option<usize>) -> Result<bool> {
        let (Some(sid), Some((used, size))) = (self.session_id.clone(), self.context_usage()) else { return Ok(false) };
        let (high, mut full) = pressure(used, size, u64::try_from(incoming.unwrap_or(0)).unwrap_or(u64::MAX));
        let c = self.conversation(&sid);
        // 结果元数据暂时写不进去时仍保留本进程的尝试，避免每次补查重新付费整理。
        let attempt = if let Some(attempt) = self.context_attempts.get(&sid) {
            Some(attempt.clone())
        } else {
            self.hub.store.kv_read(&self.attempt_key(&sid))?.map(|v| serde_json::from_str::<Attempt>(&v)).transpose()?
        };
        // 原生整理确认后但尚未收到新 usage，旧占用已过期，不能据此误报仍然满窗。
        let stale = attempt
            .as_ref()
            .is_some_and(|a| a.confirmed && self.context_usage_at.get(&sid).copied().unwrap_or(0) <= a.started);
        if stale {
            // 当前占用未知，但任务包本身超过已知窗口仍可判定不可容纳；不能把未知当零发送。
            let minimum = u64::try_from(incoming.unwrap_or(0)).unwrap_or(u64::MAX);
            let reserve = (size / 16).clamp(2_048, 8_192);
            if incoming.is_none() || size == 0 || minimum.saturating_add(reserve) <= size {
                return Ok(false);
            }
            full = true;
        }
        let suppressed = attempt.as_ref().is_some_and(|a| {
            a.epoch == c.compactions && a.blocked && a.used == used && used >= ((size / 4 * 3).min(CONTEXT_TARGET)) / 2
        });
        if (high || full) && !stale && self.compact_command().is_some() && !suppressed {
            if !self.queue.iter().any(|q| matches!(q, Queued::Compact { .. })) {
                self.queue.push_back(Queued::Compact {
                    idle: false,
                    group: Some(Maintenance {
                        room: d.room_id.clone(),
                        delivery: d.id.clone(),
                        session: sid,
                        used,
                        epoch: c.compactions,
                        started: now_ms(),
                        confirmed: false,
                        deadline: Instant::now() + MAINTENANCE_TIMEOUT,
                        timed_out: false,
                    }),
                });
            }
            return Ok(true);
        }
        // 只在已准备完整任务包后判断空间；后台预算不足永不取消投递。
        full &= incoming.is_some();
        if full {
            let reported = if stale {
                format!("整理后占用尚未知，窗口 {size}")
            } else {
                format!("当前会话已用 {used}/{size}")
            };
            let detail = format!(
                "上下文空间不足：已保留原始任务、要求和待办，派工仍在排队。{reported}，新增任务包估算约 {} token（不是实测）。请在成员设置中选择更大窗口的模型，或在确认后使用 /new 新建会话；条件变化后会自动继续。",
                incoming.unwrap_or_default()
            );
            self.hub.delivery_context_problem(&d.id, &self.cfg.id, &detail)?;
            crate::analytics::capture(&self.hub, crate::analytics::Event::GroupContextPrepared,
                serde_json::json!({"outcome":"queued_insufficient_context","protected_estimated_tokens":incoming,"background_budget":crate::chat::group::background_budget(),"context_used":(!stale).then_some(used),"context_size":size}).as_object().cloned().unwrap_or_default());
            self.context_blocks.insert(
                d.id.clone(),
                Blocked {
                    session: self.session_id.clone(),
                    model: self.context_model(),
                    room_rev: self.hub.store.group_context_revision(&d.room_id)?,
                    usage: self.context_usage(),
                },
            );
            return Ok(true);
        }
        Ok(false)
    }

    /// 执行前再次确认仍是同一个 queued 请求与会话，防止 Stop、移除或新会话后的旧维护抢跑。
    pub(super) async fn start_group_compaction(
        &mut self,
        mut group: Maintenance,
        done_tx: &mpsc::UnboundedSender<Done>,
    ) {
        let valid = match self.hub.store.delivery(&group.delivery) {
            Ok(Some(d)) => d.status == Status::Queued && d.target_bot_id == self.cfg.id && d.room_id == group.room,
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(%error, "context maintenance source lookup failed; original task stays queued");
                return;
            }
        };
        if !valid
            || self.session_id.as_deref() != Some(group.session.as_str())
            || self.pending_completion.is_some()
            || self.pending_new_session.is_some()
        {
            return;
        }
        if self.compact_command().is_none() {
            return;
        }
        group.started = now_ms();
        group.deadline = Instant::now() + MAINTENANCE_TIMEOUT;
        let attempt =
            Attempt { epoch: group.epoch, used: group.used, blocked: true, confirmed: false, started: group.started };
        let saved = serde_json::to_string(&attempt)
            .map_err(anyhow::Error::from)
            .and_then(|v| self.hub.store.kv_set(&self.attempt_key(&group.session), &v));
        if let Err(error) = saved {
            tracing::warn!(%error,"could not record context maintenance attempt");
            return;
        }
        self.context_attempts.insert(group.session.clone(), attempt);
        self.context_blocks.remove(&group.delivery);
        self.group_compaction = Some(group);
        self.start_compaction(false, done_tx).await;
    }

    /// 整理卡住时只取消维护；排队的业务请求没有 started，也不会被重放。
    pub(super) async fn expire_group_compaction(&mut self) {
        let due = self.group_compaction.as_ref().is_some_and(|g| !g.timed_out && Instant::now() >= g.deadline);
        if due {
            if let Some(g) = self.group_compaction.as_mut() {
                g.timed_out = true;
            }
            self.cancel_turn().await;
        }
    }

    /// 维护结束补记启动失败的未知用量，只更新私有检查点与状态，不提交业务结果或派工。
    pub(super) fn finish_group_compaction(&mut self, done: &Done) {
        // 会话重载失败可在 ACP prompt 回调前结束；take 保证正常完成回调已经记账时不重复记录。
        self.record_usage(done);
        self.after_turn(done);
        self.close_attempt(done.is_err() || self.stop_requested);
        let Some(group) = self.group_compaction.take() else { return };
        let current = self.conn.as_ref().and_then(|c| c.usage.get(&group.session)).copied();
        let confirmed = group.confirmed || current.is_some_and(|(used, _)| used < group.used / 2);
        let success =
            done.as_ref().is_ok_and(|v| v["stopReason"] == "end_turn") && !self.stop_requested && !group.timed_out;
        let epoch = self.conversation(&group.session).compactions;
        let gained = current.is_some_and(|(used, size)| used < group.used && used < (size / 4 * 3).min(CONTEXT_TARGET));
        let attempt = Attempt {
            epoch,
            used: current.map_or(group.used, |v| v.0),
            blocked: !success || !gained,
            confirmed: success && confirmed,
            started: group.started,
        };
        self.context_attempts.insert(group.session.clone(), attempt.clone());
        if let Err(error) = serde_json::to_string(&attempt)
            .map_err(anyhow::Error::from)
            .and_then(|v| self.hub.store.kv_set(&self.attempt_key(&group.session), &v))
        {
            tracing::warn!(%error,"could not keep maintenance outcome");
        }
        let reported_after = self
            .context_usage_at
            .get(&group.session)
            .is_some_and(|at| *at > group.started)
            .then_some(current)
            .flatten();
        crate::analytics::capture(&self.hub,crate::analytics::Event::GroupContextCompacted,
            serde_json::json!({"outcome":if group.timed_out {"timeout"} else if self.stop_requested {"cancelled"} else if !success {"failed"} else if confirmed {"confirmed"} else {"unconfirmed"},"context_before":group.used,"context_after":reported_after.map(|v|v.0),"context_size":reported_after.map(|v|v.1)}).as_object().cloned().unwrap_or_default());
        // 已送达的 profile 水位照常提交；维护本身不是用户发言或记忆来源。
        if let (Ok(_), Some((snapshot, identity))) = (done, self.announce.take())
            && let Err(error) = context::mark_announced(&self.hub.store, &self.cfg.id, &snapshot, identity)
        {
            tracing::warn!(%error,"maintenance opening not recorded");
        }
        if let (Ok(_), Some(sent)) = (done, self.first_prompt.take())
            && let Err(error) = context::mark_prompted(&self.hub.store, &self.cfg.id, &sent)
        {
            tracing::warn!(%error,"maintenance profile not recorded");
        }
        if let (Some(conn), Some(sid)) = (self.conn.as_mut(), &self.turn_session) {
            // 整理同样使用了连接；不能立即按旧的最后使用时间关闭它。
            conn.loaded.insert(sid.clone(), Instant::now());
        }
        self.turn = None;
        self.turn_text = None;
        self.turn_entries.clear();
        self.turn_call = None;
        self.reply_hops = None;
        self.compacting = None;
        self.compaction_notice = None;
        self.raw_prompt = false;
        self.cancel_deadline = None;
        self.hub.team.end_turn(&self.cfg.id);
        self.hub.set_runtime(&self.cfg.id, |r| {
            r.status = if r.storage_error.is_some() { BotStatus::Error } else { BotStatus::Idle };
            r.activity = r.storage_error.clone().unwrap_or_default();
            r.started_at = None;
            if r.storage_error.is_none() {
                r.lane = None;
            }
            r.message = None;
            r.session = None;
        });
        // 状态和原文始终由持久投递恢复，wake 仅重新给予调度机会。
        crate::chat::group::wake(&self.hub, Some(&self.cfg.id));
        let _ = self.dreamer.send(memory::DreamEvent::Flush);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未知与零不同，确认整理后的旧占用不能冒充新统计。
    #[test]
    fn reported_usage_keeps_unknown_and_stale_values_explicit() {
        assert_eq!(reported_usage(None, false), (None, None));
        assert_eq!(reported_usage(Some((42, 0)), false), (Some(42), None));
        assert_eq!(reported_usage(Some((70_000, 200_000)), true), (None, Some(200_000)));
        assert_eq!(reported_usage(Some((20_000, 200_000)), false), (Some(20_000), Some(200_000)));
    }
}
