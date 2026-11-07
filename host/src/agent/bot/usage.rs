//! 回合尝试的本地观测：只保留执行器实报，缺失不是零，共享协调开销不摊给单个请求。

use super::{Actor, Done};
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
        self.usage_attempt = Some(Attempt {
            id: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            agent: self.cfg.backend.clone(),
            model: self.cfg.model.clone(),
        });
    }

    /// 在回退前收束一次尝试，确保失败、取消和缺少实报的尝试也留下未知记录。
    pub(super) fn record_usage(&mut self, done: &Done) {
        let Some(attempt) = self.usage_attempt.take() else { return };
        let request = self.active_request.as_ref().map(crate::chat::team::BotRequest::request_id);
        let (scope, requests) = association(request, &self.reply_requests);
        // 完成结果缺失时保留未知；不能把失败尝试当作免费。
        let usage = done.as_ref().ok().map_or_else(|| reported(&Value::Null), |answer| reported(&answer["usage"]));
        let outcome = if self.stop_requested || done.as_ref().is_ok_and(|v| v["stopReason"] == "cancelled") {
            "cancelled"
        } else if done.is_err() {
            "failed"
        } else if done.as_ref().is_ok_and(|v| v["stopReason"].as_str().is_none_or(|s| s == "end_turn")) {
            "completed"
        } else {
            "incomplete"
        };
        // 标识只进入本机日志；不发送 PostHog，也不按账户窗口伪造任务费用。
        tracing::info!(
            bot = %self.cfg.id, session = ?self.turn_session, turn = ?self.turn,
            agent = %attempt.agent, model = ?attempt.model,
            attempt = %attempt.id, elapsed_ms = attempt.started.elapsed().as_millis(),
            scope, request_ids = %json!(requests), outcome, usage = %usage,
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
    let mut result = json!({"source": "unavailable"});
    let mut available = false;
    for field in ["inputTokens", "outputTokens", "cachedReadTokens", "cachedWriteTokens", "thoughtTokens"] {
        let count = usage[field].as_u64();
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
