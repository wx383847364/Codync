//! A real host and built-in MCP process, with deterministic ACP agents instead
//! of paid providers. Exercises discovery, messages, approval and the reply's way back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Child;
use std::time::Duration;

struct Host {
    child: Child,
    home: PathBuf,
    base: String,
    token: String,
}

impl Drop for Host {
    fn drop(&mut self) {
        common::stop(&mut self.child);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Host {
    /// 启动独立 Host 并显式开启信息日志，避免继承外部 warn 过滤而漏验用量。
    async fn start() -> Self {
        let home = std::env::temp_dir().join(format!("codync-team-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (child, base) = common::serve_with(&home, &[("RUST_LOG", std::ffi::OsStr::new("codync_host=info"))]).await;
        let mut host = Self { child, home, base, token: String::new() };
        std::fs::read_to_string(host.home.join("token")).unwrap().trim().clone_into(&mut host.token);
        host
    }

    async fn call(&self, method: &str, body: Value) -> Value {
        let response = reqwest::Client::new()
            .post(format!("{}/api/{method}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            // As long as `common::host`'s: on a busy machine (parallel tests) `createBot` alone can take over 10 s.
            .timeout(Duration::from_mins(2))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{method}: {value}");
        value
    }

    async fn bot(&self, name: &str) -> String {
        let cwd = self.home.join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team_agent.py");
        let result = self
            .call(
                "createBot",
                json!({
                    "name": name, "backend": "custom", "cwd": cwd, "permission": "ask", "notify": false,
                    "command": common::python_agent(&agent),
                }),
            )
            .await;
        result["bot"]["id"].as_str().unwrap().to_owned()
    }

    async fn wait_for(&self, bot: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let history = self.call("history", json!({"botId": bot})).await;
                if let Some(entry) = history["entries"].as_array().unwrap().iter().find(|e| predicate(e)) {
                    return entry.clone();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("team flow did not finish")
    }

    async fn wait_finished(&self) -> Value {
        let completed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let sync = common::snapshot(&self.base, &self.token).await;
                let mut notices =
                    sync["entries"].as_array().unwrap().iter().filter(|e| e["data"]["delegationId"].is_string());
                if sync["bots"].as_array().unwrap().iter().all(|b| b["status"] == "idle")
                    && notices.all(|e| e["data"]["status"] == "completed")
                {
                    return sync;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        completed.expect("team flow did not release its turns and notices")
    }

    fn fixture_log(&self, bot: &str, file: &str) -> Vec<Value> {
        std::fs::read_to_string(self.home.join(bot).join(file))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn finals(history: &Value) -> Vec<&str> {
    history["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "agent" && e["data"]["final"] == true)
        .map(|e| e["data"]["text"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_bot_messages_another_and_relays_its_reply_to_the_user() {
    let host = Host::start().await;
    let lead = host.bot("lead").await;
    let reviewer = host.bot("reviewer").await;
    host.call("send", json!({"botId": lead, "text": format!("DELEGATE {reviewer}")})).await;
    let permission = host.wait_for(&reviewer, |e| e["kind"] == "permission" && e["data"]["status"] == "pending").await;
    // The lead didn't wait: its turn ended as soon as the request was queued.
    let queued = host.wait_for(&lead, |e| e["kind"] == "agent" && e["data"]["final"] == true).await;
    assert_eq!(queued["data"]["text"], "Message queued");
    // Stopping the sender leaves the recipient's approval and the accepted request alone.
    host.call("stop", json!({"botId": lead})).await;
    host.call("respondPermission", json!({"entryId": permission["id"], "optionId": "allow"})).await;
    let relayed = host
        .wait_for(&lead, |e| {
            e["kind"] == "agent" && e["data"]["final"] == true && e["data"]["text"] != "Message queued"
        })
        .await;
    assert_eq!(relayed["data"]["text"], "Relayed: reply: PERMISSION review the changes");
    let sync = host.wait_finished().await;
    let requests: Vec<_> =
        sync["entries"].as_array().unwrap().iter().filter(|e| e["data"]["delegationId"].is_string()).collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["data"]["delegationId"], requests[1]["data"]["delegationId"]);
    for entry in &requests {
        assert_eq!(entry["kind"], "notice");
        assert_eq!(entry["data"]["sourceBotId"], lead);
        assert_eq!(entry["data"]["targetBotId"], reviewer);
        assert_eq!(entry["data"]["status"], "completed");
        assert_eq!(entry["data"]["botMessage"]["reply"], "reply: PERMISSION review the changes");
    }
    let conversation = host.call("botConversation", json!({"botId": lead, "peerId": reviewer})).await;
    assert_eq!(conversation["entries"].as_array().unwrap().len(), 1);
    let recipient_history = host.call("history", json!({"botId": reviewer})).await;
    assert_eq!(finals(&recipient_history), Vec::<&str>::new(), "the answer is the lead's, not the reviewer's chat's");
    assert!(recipient_history["entries"].as_array().unwrap().iter().all(|e| e["kind"] != "user"));
    assert!(
        recipient_history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "agent" && e["data"]["text"] == "reply: PERMISSION review the changes"),
        "the reply remains available in the trace"
    );
    let lead_history = host.call("history", json!({"botId": lead})).await;
    assert_eq!(finals(&lead_history), ["Message queued", "Relayed: reply: PERMISSION review the changes"]);
    let prompts = host.fixture_log("lead", "prompts.jsonl");
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].as_str().unwrap().contains("[Codync: reviewer replied to your message_bot request"));
}

#[tokio::test]
async fn a_bot_request_answers_the_sender_and_never_the_user() {
    let host = Host::start().await;
    let lead = host.bot("lead").await;
    let reviewer = host.bot("reviewer").await;
    host.call("send", json!({"botId": lead, "text": format!("DELEGATE_REPORT {reviewer}")})).await;
    host.wait_for(&lead, |e| e["kind"] == "agent" && e["data"]["text"] == "Relayed: Answer for the requesting bot")
        .await;
    host.wait_finished().await;
    for bot in [&lead, &reviewer] {
        let history = host.call("history", json!({"botId": bot})).await;
        let notices: Vec<_> = history["entries"].as_array().unwrap().iter().filter(|e| e["kind"] == "notice").collect();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0]["data"]["status"], "completed");
        assert!(notices[0]["data"]["text"].as_str().unwrap().contains("Answer for the requesting bot"));
        assert!(!history["entries"].as_array().unwrap().iter().any(|e| e["data"]["text"] == "Report for the user"));
        if bot == &reviewer {
            assert_eq!(finals(&history), Vec::<&str>::new());
        }
    }
}

#[tokio::test]
async fn user_report_queued_behind_a_request_is_delivered_in_the_recipient_chat() {
    let host = Host::start().await;
    let lead = host.bot("lead").await;
    let reviewer = host.bot("reviewer").await;
    host.call("send", json!({"botId": lead, "text": format!("DELEGATE {reviewer}")})).await;
    let permission = host.wait_for(&reviewer, |e| e["kind"] == "permission" && e["data"]["status"] == "pending").await;
    host.call("send", json!({"botId": reviewer, "text": "REPORT"})).await;
    host.call("respondPermission", json!({"entryId": permission["id"], "optionId": "allow"})).await;
    host.wait_for(&reviewer, |e| {
        e["kind"] == "agent" && e["data"]["final"] == true && e["data"]["text"] == "Second report for the user"
    })
    .await;
    host.wait_for(&lead, |e| e["data"]["text"] == "Relayed: reply: PERMISSION review the changes").await;
    let sync = host.wait_finished().await;
    // The request's limits were its turn's: the user's own turn after it reports as usual.
    let entries: Vec<_> = sync["entries"].as_array().unwrap().iter().filter(|e| e["botId"] == reviewer).collect();
    let reports: Vec<_> = entries
        .iter()
        .filter(|e| e["kind"] == "agent" && e["data"]["final"] == true)
        .map(|e| e["data"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(reports, ["Report for the user", "Second report for the user"]);
    assert!(entries.iter().any(|e| {
        e["kind"] == "agent" && e["data"]["final"] != true && e["data"]["text"] == "Private completion trace"
    }));
    let prompts = host.fixture_log("reviewer", "prompts.jsonl");
    assert_eq!(prompts.len(), 2);
    assert!(prompts[0].as_str().unwrap().contains("Your final reply goes back to lead"));
    assert_eq!(prompts[1], "REPORT");
    assert_eq!(host.fixture_log("reviewer", "sessions.jsonl").len(), 1, "the existing session must be reused");
}

#[tokio::test]
async fn message_ping_pong_stops_at_the_hop_limit_and_a_new_user_turn_can_delegate() {
    let host = Host::start().await;
    let first = host.bot("first").await;
    let second = host.bot("second").await;
    for (name, target) in [("first", &second), ("second", &first)] {
        std::fs::write(host.home.join(name).join("handoff.json"), json!({"target": target}).to_string()).unwrap();
    }
    let mut previous_rev = 0;
    // `first` passes the chain on from each reply turn and `second` hands it back, so only the hops each reply
    // carries stop it. The second user turn must start a fresh budget after the first chain ends.
    for chain in 1..=2 {
        host.call("send", json!({"botId": first, "text": "CHAIN"})).await;
        let stopped = host
            .wait_for(&first, |e| {
                e["rev"].as_i64().unwrap_or(0) > previous_rev
                    && e["kind"] == "agent"
                    && e["data"]["text"].as_str().is_some_and(|s| s.contains("hop limit (8)"))
            })
            .await;
        assert!(stopped["data"]["text"].as_str().unwrap().contains("Chain stopped"));
        let sync = host.wait_finished().await;
        // The streamed text can precede finish_turn. Read its final state only
        // after both bots and all request notices have completed.
        let entries = sync["entries"].as_array().unwrap();
        let completed = entries.iter().find(|e| e["id"] == stopped["id"]).expect("completed stop entry");
        assert_eq!(completed["data"]["final"], true, "a reply turn answers the user");
        let notices = entries.iter().filter(|e| e["data"]["delegationId"].is_string()).count();
        assert_eq!(notices, chain * 8 * 2, "the ninth handoff must never be queued");
        previous_rev = sync["rev"].as_i64().unwrap();
    }
}

/// 验证长回复保留正文传递与重启后的 MCP 精确分页，不因读取重新执行委派。
#[tokio::test]
async fn completed_local_reply_is_readable_through_mcp_after_restart() {
    let mut host = Host::start().await;
    let lead = host.bot("lead").await;
    let reviewer = host.bot("reviewer").await;
    host.call("send", json!({"botId": lead, "text": format!("DELEGATE {reviewer} LONG_REPLY")})).await;
    // 完成通知与来源一起保存后才能读取，而非依据流式文本提前取回。
    let notice = host
        .wait_for(&lead, |entry| entry["data"]["status"] == "completed" && entry["data"]["delegationId"].is_string())
        .await;
    host.wait_finished().await;
    let request = notice["data"]["delegationId"].as_str().unwrap();
    let before = host.fixture_log("reviewer", "prompts.jsonl");
    let expected =
        format!("结论：已完成\n{}中间证据{}\n未验证：真实提供方费用", "甲🙂".repeat(2200), "乙".repeat(2200));
    // 这段正文超过旧预览阈值但未到原传递上限，必须一次交付全部中间证据。
    let delivered = host.fixture_log("lead", "prompts.jsonl");
    assert!(delivered.iter().any(|prompt| prompt.as_str().is_some_and(|text| text.contains(&expected))));
    let mut offset = 0;
    let mut body = String::new();
    loop {
        let result = host.call("teamCall", json!({"botId": lead, "name": "read_bot_reply", "arguments": {"requestId": request, "offset": offset, "limit": 1000}})).await;
        assert_eq!(result["source"], "trace");
        assert_eq!(result["complete"], true);
        body.push_str(result["text"].as_str().unwrap());
        let Some(next) = result["nextOffset"].as_u64() else { break };
        offset = next;
    }
    assert_eq!(body, expected);
    let log = std::fs::read_to_string(host.home.join("host-stdout.log")).unwrap();
    assert!(log.contains("request_execution"));
    assert!(log.contains("provider_reported"));
    assert!(log.contains("outputTokens"));
    assert!(log.contains("shared_coordinator"));
    assert!(log.contains("unavailable"));
    common::stop(&mut host.child);
    let (child, base) = common::serve_with(&host.home, &[("RUST_LOG", std::ffi::OsStr::new("codync_host=info"))]).await;
    host.child = child;
    host.base = base;
    host.call("send", json!({"botId": lead, "text": format!("READ_REPLY {}", json!({"requestId": request, "offset": 4000, "limit": 1000}))})).await;
    // 假执行器把真正 MCP content 文本作为最终回复，核对模型可见的分页。
    let entry = host
        .wait_for(&lead, |entry| {
            entry["kind"] == "agent"
                && entry["data"]["final"] == true
                && entry["data"]["text"].as_str().is_some_and(|text| text.contains("totalCharacters"))
        })
        .await;
    let page: Value = serde_json::from_str(entry["data"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(page["text"], expected.chars().skip(4000).take(1000).collect::<String>());
    assert_eq!(page["complete"], true);
    assert_eq!(host.fixture_log("reviewer", "prompts.jsonl"), before, "reading never replays the worker");
}

/// 执行中重配不改变旧模型归属，取消只记一次且不会伪造零消费。
#[tokio::test]
async fn usage_keeps_running_model_and_records_cancellation_once() {
    let host = Host::start().await;
    let bot = host.bot("usage-model").await;
    host.call("updateBot", json!({"id": bot, "model": "old"})).await;
    host.call("send", json!({"botId": bot, "text": "BLOCK snapshot"})).await;
    // 等待旧模型真实开始执行，再修改未来回合使用的配置。
    common::eventually("old model running", Duration::from_secs(20), async || {
        host.home.join("usage-model/prompts.jsonl").exists()
    })
    .await;
    host.call("updateBot", json!({"id": bot, "model": "new"})).await;
    std::fs::write(host.home.join("usage-model/release"), "").unwrap();
    host.wait_for(&bot, |e| e["data"]["final"] == true && e["data"]["text"] == "reply: BLOCK snapshot").await;
    host.call("send", json!({"botId": bot, "text": "next"})).await;
    host.wait_for(&bot, |e| e["data"]["final"] == true && e["data"]["text"] == "reply: next").await;
    std::fs::remove_file(host.home.join("usage-model/release")).unwrap();
    host.call("send", json!({"botId": bot, "text": "BLOCK cancel"})).await;
    // 第二次阻塞开始后取消，不能仅观察上一回合的已完成记录。
    common::eventually("third prompt running", Duration::from_secs(20), async || {
        host.fixture_log("usage-model", "prompts.jsonl").len() == 3
    })
    .await;
    host.call("stop", json!({"botId": bot})).await;
    // 等待取消尝试记录到磁盘，再核对每个 attempt 仅出现一次。
    common::eventually("cancel usage", Duration::from_secs(20), async || {
        common::turn_usage(&host.home)
            .iter()
            .any(|r| r.get("bot") == Some(&bot) && r.get("outcome").is_some_and(|v| v == "cancelled"))
    })
    .await;
    let rows: Vec<_> = common::turn_usage(&host.home).into_iter().filter(|r| r.get("bot") == Some(&bot)).collect();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0]["model"], "Some(\"old\")");
    assert_eq!(rows[1]["model"], "Some(\"new\")");
    assert_eq!(rows[2]["outcome"], "cancelled");
    let attempts: std::collections::BTreeSet<_> = rows.iter().map(|r| &r["attempt"]).collect();
    assert_eq!(attempts.len(), 3);
    assert!(rows.iter().all(|r| r["usage"].contains("unavailable")));
}

/// 失败与回退成功属于两个不同尝试，启动失败也留下明确的未知消费。
#[tokio::test]
async fn usage_records_failed_fallback_and_start_failure() {
    let host = Host::start().await;
    let bot = host.bot("usage-fallback").await;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/relay_agent.py");
    let command = common::python_agent(&script);
    host.call("updateBot", json!({"id": bot, "command": format!("{command} primary"), "fallbacks": [{"backend": "codex", "command": format!("{command} standin")}]})).await;
    std::fs::write(host.home.join("usage-fallback/primary.limit"), "").unwrap();
    host.call("send", json!({"botId": bot, "text": "fallback"})).await;
    // 最终答复完成时，前一个失败尝试已在切换身份前记录。
    host.wait_for(&bot, |e| e["data"]["final"] == true && e["data"]["text"] == "standin answered").await;
    let rows: Vec<_> = common::turn_usage(&host.home).into_iter().filter(|r| r.get("bot") == Some(&bot)).collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["agent"], "custom");
    assert_eq!(rows[0]["outcome"], "failed");
    assert_eq!(rows[1]["agent"], "codex");
    assert_eq!(rows[1]["outcome"], "completed");
    assert_ne!(rows[0]["attempt"], rows[1]["attempt"]);
    let broken = host.bot("usage-start-failure").await;
    host.call("updateBot", json!({"id": broken, "command": "codync-test-command-does-not-exist"})).await;
    host.call("send", json!({"botId": broken, "text": "start"})).await;
    host.wait_for(&broken, |e| e["data"]["style"] == "error").await;
    let rows: Vec<_> = common::turn_usage(&host.home).into_iter().filter(|r| r.get("bot") == Some(&broken)).collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["outcome"], "failed");
    assert!(rows[0]["usage"].contains("unavailable"));
}

/// 两个回复汇入一个主脑回合时保留整组请求标识，不拆成虚假的逐任务消费。
#[tokio::test]
async fn usage_keeps_batched_reply_associations_together() {
    let host = Host::start().await;
    let lead = host.bot("usage-lead").await;
    let one = host.bot("usage-one").await;
    let two = host.bot("usage-two").await;
    host.call("send", json!({"botId": lead, "text": "BLOCK coordinator"})).await;
    // 主脑先保持工作，使两条回复确定性地排进同一后续回合。
    common::eventually("coordinator blocked", Duration::from_secs(20), async || {
        host.home.join("usage-lead/prompts.jsonl").exists()
    })
    .await;
    let first = host
        .call("teamCall", json!({"botId": lead, "name": "message_bot", "arguments": {"botId": one, "message": "one"}}))
        .await;
    let second = host
        .call("teamCall", json!({"botId": lead, "name": "message_bot", "arguments": {"botId": two, "message": "two"}}))
        .await;
    // 两个执行者都完成后才释放主脑，避免调度时间影响批量归属断言。
    host.wait_for(&lead, |e| e["data"]["delegationId"] == first["requestId"] && e["data"]["status"] == "completed")
        .await;
    host.wait_for(&lead, |e| e["data"]["delegationId"] == second["requestId"] && e["data"]["status"] == "completed")
        .await;
    std::fs::write(host.home.join("usage-lead/release"), "").unwrap();
    host.wait_for(&lead, |e| {
        e["data"]["final"] == true
            && e["data"]["text"].as_str().is_some_and(|s| s.contains("reply: one") && s.contains("reply: two"))
    })
    .await;
    let rows: Vec<_> = common::turn_usage(&host.home)
        .into_iter()
        .filter(|r| r.get("bot") == Some(&lead) && r.get("scope").is_some_and(|s| s == "shared_coordinator"))
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let ids: Vec<String> = serde_json::from_str(&rows[0]["request_ids"]).unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&first["requestId"].as_str().unwrap().to_owned()));
    assert!(ids.contains(&second["requestId"].as_str().unwrap().to_owned()));
}
