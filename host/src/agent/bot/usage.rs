//! 回合尝试的本地观测：只保留执行器实报，缺失不是零，共享协调开销不摊给单个请求。

use super::{Actor, Done};
use crate::store::turn_usage::{AttemptStart, TOKEN_FIELDS, UsageEvent};
use serde_json::{Value, json};
use std::time::Instant;

pub(super) struct Attempt {
    id: String,
    started: Instant,
    agent: String,
    model: Option<String>,
}

impl Actor {
    /// 在执行器选择后固定本次身份，运行中重新配置不能改变旧尝试的归属。
    pub(super) fn begin_usage(&mut self) {
        let attempt = Attempt {
            id: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            agent: self.cfg.backend.clone(),
            model: self.cfg.model.clone(),
        };
        let delivery = self.active_delivery.as_ref().map(|active| &active.delivery);
        let room = delivery
            .map(|row| row.room_id.as_str())
            .or_else(|| self.group_compaction.as_ref().map(|group| group.room.as_str()));
        let request = self.active_request.as_ref().map(crate::chat::team::BotRequest::request_id);
        let (scope, requests) = association(request, &self.reply_requests);
        let source = if self.group_compaction.is_some() || self.compacting.is_some() {
            "maintenance"
        } else if self.active_routine.is_some() {
            "routine"
        } else if let Some(row) = delivery {
            row.source_kind.as_str()
        } else if scope != "other" {
            scope
        } else if self.active_prompt {
            "host"
        } else {
            "private"
        };
        if let Err(error) = self.hub.store.begin_usage_attempt(&AttemptStart {
            id: &attempt.id,
            bot: &self.cfg.id,
            room,
            delivery: delivery.map(|row| row.id.as_str()),
            backend: &attempt.agent,
            source,
            requests: &requests,
        }) {
            tracing::warn!(%error, "could not persist usage attempt start");
        }
        self.usage_attempt = Some(attempt);
    }

    /// 从会话返回或配置更新中提取实际模型，缺少报告时不猜测。
    pub(super) fn note_usage_model(&mut self, sid: &str, options: &Value) {
        let model = options["_codyncSelectedModel"]
            .as_str()
            .or_else(|| super::launch::reported_model(options))
            .filter(|model| !model.is_empty());
        if let (Some(conn), Some(model)) = (self.conn.as_mut(), model) {
            conn.confirmed_models.insert(sid.to_owned(), model.to_owned());
        }
    }

    /// 只累计已发生的观测事件，写入失败不改变任务本身的执行。
    pub(super) fn usage_event(&self, event: UsageEvent) {
        let Some(attempt) = &self.usage_attempt else { return };
        let model = match event {
            UsageEvent::Prompt => self
                .turn_session
                .as_ref()
                .and_then(|sid| self.conn.as_ref()?.confirmed_models.get(sid))
                .map(String::as_str),
            UsageEvent::Session | UsageEvent::Process => None,
        };
        if let Err(error) = self.hub.store.observe_usage_event(&attempt.id, event, model) {
            tracing::warn!(%error, "could not persist usage observation");
        }
    }

    /// 在回退前记录尝试及本群归属，失败、取消和缺报保留真实状态。
    pub(super) fn record_usage(&mut self, done: &Done) {
        self.record_usage_outcome(done, false);
    }

    /// 群内无回复也可有实报消耗，使用已计算的最终交付状态。
    pub(super) fn record_usage_outcome(&mut self, done: &Done, no_reply: bool) {
        self.finish_usage_record(done, no_reply, false);
    }

    /// Host 主动退出中断运行中的尝试，区别于用户取消。
    pub(super) fn interrupt_usage(&mut self) {
        self.finish_usage_record(&Err(anyhow::anyhow!("shutdown")), false, true);
    }

    /// 收束持久尝试和现有日志，终态以本次调用发生的原因区分。
    fn finish_usage_record(&mut self, done: &Done, no_reply: bool, interrupted: bool) {
        let Some(attempt) = self.usage_attempt.take() else { return };
        let request = self.active_request.as_ref().map(crate::chat::team::BotRequest::request_id);
        let (scope, requests) = association(request, &self.reply_requests);
        let delivery = self.active_delivery.as_ref().map(|active| &active.delivery);
        // 完成结果缺失时保留未知；不能把失败尝试当作免费。
        let usage = done.as_ref().ok().map_or_else(|| reported(&Value::Null), |answer| reported(&answer["usage"]));
        let outcome = if interrupted {
            "interrupted"
        } else if self.stop_requested || done.as_ref().is_ok_and(|v| v["stopReason"] == "cancelled") {
            "cancelled"
        } else if done.is_err() {
            "failed"
        } else if no_reply {
            "noReply"
        } else if done.as_ref().is_ok_and(|v| v["stopReason"].as_str().is_none_or(|s| s == "end_turn")) {
            "completed"
        } else {
            "incomplete"
        };
        let elapsed = i64::try_from(attempt.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        if let Err(error) = self.hub.store.finish_usage_attempt(&attempt.id, outcome, elapsed, &usage) {
            tracing::warn!(%error, "could not persist usage attempt result");
        }
        // 标识只进入本机日志；不发送 PostHog，也不按账户窗口伪造任务费用。
        tracing::info!(
            bot = %self.cfg.id, session = ?self.turn_session, turn = ?self.turn,
            agent = %attempt.agent, model = ?attempt.model,
            attempt = %attempt.id, elapsed_ms = attempt.started.elapsed().as_millis(),
            scope, request_ids = %json!(requests), room = ?delivery.map(|row| &row.room_id),
            delivery = ?delivery.map(|row| &row.id), outcome, usage = %usage,
            "turn usage"
        );
    }
}

/// 请求执行归属单个请求；批量主脑回合保留整组关联，不按字数或数量分摊。
fn association<'a>(request: Option<&'a str>, replies: &'a [String]) -> (&'static str, Vec<&'a str>) {
    if let Some(request) = request {
        ("request_execution", vec![request])
    } else if replies.is_empty() {
        ("other", vec![])
    } else {
        ("shared_coordinator", replies.iter().map(String::as_str).collect())
    }
}

/// 只接受非负整数实报；缺失、畸形、推理量或金额不从其他字段推算。
fn reported(usage: &Value) -> Value {
    // 明确标成累计量的回报不能按回合累计，未知边界时保持缺报。
    let usage = if usage["cumulative"] == true
        || matches!(usage["scope"].as_str(), Some("session" | "account" | "cumulative"))
    {
        &Value::Null
    } else {
        usage
    };
    let mut result = json!({"source": "unavailable"});
    let mut available = false;
    for (field, _) in TOKEN_FIELDS {
        let count = usage[field].as_u64().and_then(|count| i64::try_from(count).ok());
        available |= count.is_some();
        result[field] = json!(count);
    }
    if available {
        result["source"] = "provider_reported".into();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺失与非法值保持未知，真实零值则保留为实报。
    #[test]
    fn missing_usage_is_unknown_not_zero() {
        let value = reported(&json!({"inputTokens": 0, "outputTokens": -1, "cachedReadTokens": "10"}));
        assert_eq!(value["source"], "provider_reported");
        assert_eq!(value["inputTokens"], 0);
        assert!(value["outputTokens"].is_null());
        assert!(value["cachedReadTokens"].is_null());
        assert!(value["cachedWriteTokens"].is_null());
        assert_eq!(reported(&Value::Null)["source"], "unavailable");
        assert_eq!(reported(&json!({"used": 123, "size": 200_000}))["source"], "unavailable");
        assert_eq!(reported(&json!({"inputTokens": 500, "scope": "session"}))["inputTokens"], Value::Null);
    }

    /// 协调回合保留批量归属，不制造每任务精确消费量。
    #[test]
    fn batched_replies_keep_shared_costs() {
        let replies = vec!["one".into(), "two".into()];
        assert_eq!(association(None, &replies), ("shared_coordinator", vec!["one", "two"]));
        assert_eq!(association(Some("request"), &replies), ("request_execution", vec!["request"]));
        assert_eq!(association(None, &[]), ("other", vec![]));
    }
}
