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
    async fn start() -> Self {
        let home = std::env::temp_dir().join(format!("codync-team-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (child, base) = common::serve(&home).await;
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

/// 真正杀掉 host 子进程并用原数据目录重启，不能用丢弃 Hub 模拟崩溃。
async fn restart_group_host(host: &mut Host) {
    host.child.kill().unwrap();
    host.child.wait().unwrap();
    let (child, base) = common::serve(&host.home).await;
    host.child = child;
    host.base = base;
}

/// 查询来源事件的聚合卡，避免按 UUID 或当前显示名猜测投递身份。
async fn group_card(host: &Host, room: &str, source: &str, status: &str) -> Value {
    host.wait_for(room, |e| {
        e["id"] == format!("delivery-card-{source}-1")
            && e["data"]["deliveryRows"].as_array().is_some_and(|rows| rows.iter().all(|row| row["status"] == status))
    })
    .await
}

/// 同名成员选人、UTF-16裁剪、后续改名与两个忙碌请求共用真实agent流程。
#[tokio::test]
async fn selected_group_identity_and_two_busy_requests_survive_rename() {
    let host = Host::start().await;
    let a = host.bot("A").await;
    let b = host.bot("B").await;
    for id in [&a, &b] {
        host.call("updateBot", json!({"id":id,"name":"Twin"})).await;
    }
    let room = host.call("createBot", json!({"kind":"group","name":"Room","members":[a,b]})).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    host.call("send", json!({"botId":a,"text":"BLOCK private"})).await;
    host.wait_for(&a, |e| e["kind"] == "user" && e["data"]["status"] == "sent").await;
    let mut sources = Vec::new();
    for text in ["  😀 @Twin first original  ", "  😀 @Twin second original  "] {
        let sent =
            host.call("send", json!({"botId":room,"text":text,"mentions":[{"botId":a,"start":5,"end":10}]})).await;
        assert_eq!(sent["entry"]["data"]["mentions"], json!([{"botId":a,"start":3,"end":8}]));
        let source = sent["entry"]["id"].as_str().unwrap().to_owned();
        let card = group_card(&host, &room, &source, "queued").await;
        assert_eq!(card["data"]["deliveryRows"][0]["targetBotId"], a);
        sources.push(source);
    }
    host.call("updateBot", json!({"id":a,"name":"Renamed"})).await;
    std::fs::write(host.home.join("A/release"), "").unwrap();
    for source in &sources {
        group_card(&host, &room, source, "completed").await;
    }
    let prompts = host.fixture_log("A", "prompts.jsonl");
    assert!(prompts.iter().any(|p| p.as_str().unwrap_or_default().contains("first original")));
    assert!(prompts.iter().any(|p| p.as_str().unwrap_or_default().contains("second original")));
    let private = host.call("history", json!({"botId":a})).await;
    assert!(finals(&private).iter().all(|t| !t.contains("original")), "group results never enter private chat");
}

/// queued和claimed在子进程重启后恢复，started/prompt之后只标中断，不自动重放。
#[tokio::test]
async fn group_crash_requeues_claimed_and_never_replays_started() {
    let mut host = Host::start().await;
    let a = host.bot("Held").await;
    std::fs::write(host.home.join("Held/hold-session"), "").unwrap();
    let room = host.call("createBot", json!({"kind":"group","name":"Crash room","members":[a]})).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sent = host.call("send", json!({"botId":room,"text":"first after claim"})).await;
    let source = sent["entry"]["id"].as_str().unwrap().to_owned();
    group_card(&host, &room, &source, "claimed").await;
    // 会话尚在准备，投递已claimed但prompt绝未发送。
    assert!(!host.home.join("Held/prompts.jsonl").exists());
    host.child.kill().unwrap();
    host.child.wait().unwrap();
    std::fs::remove_file(host.home.join("Held/hold-session")).unwrap();
    let (child, base) = common::serve(&host.home).await;
    host.child = child;
    host.base = base;
    group_card(&host, &room, &source, "completed").await;
    let sent = host.call("send", json!({"botId":room,"text":"BLOCK uncertain"})).await;
    let source = sent["entry"]["id"].as_str().unwrap().to_owned();
    group_card(&host, &room, &source, "started").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while host.fixture_log("Held", "prompts.jsonl").len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    restart_group_host(&mut host).await;
    let interrupted = group_card(&host, &room, &source, "interrupted").await;
    assert!(interrupted["data"]["deliveryRows"][0]["detail"].as_str().unwrap().contains("不确定"));
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(host.fixture_log("Held", "prompts.jsonl").len(), 2, "started is never automatically replayed");
    let id = interrupted["data"]["deliveryRows"][0]["deliveryId"].as_str().unwrap();
    let retry = host.call("retryDelivery", json!({"deliveryId":id})).await;
    assert_eq!(retry["delivery"]["attempt"], 2);
    assert_ne!(retry["delivery"]["id"], id);
    host.call("stop", json!({"botId":room})).await;
}

/// 三个真实ACP子进程经MCP在同群接力，工具正文不进入任何发送者私聊。
#[tokio::test]
async fn group_tools_relay_through_three_members_without_display_mentions_duplicating_work() {
    let host = Host::start().await;
    let a = host.bot("Lead").await;
    let b = host.bot("Reviewer").await;
    let c = host.bot("Verifier").await;
    std::fs::write(host.home.join("Lead/group-script.json"),json!({"tasks":[{"botId":b,"message":"review exact task body","idempotencyKey":"review-1"},{"botId":b,"message":"review exact task body","idempotencyKey":"review-1"}],"reply":"@Reviewer please review"}).to_string()).unwrap();
    std::fs::write(
        host.home.join("Reviewer/group-script.json"),
        json!({"tasks":[{"botId":c,"message":"verify the result"}],"reply":"@Verifier please verify"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        host.home.join("Verifier/group-script.json"),
        json!({"tasks":[],"reply":"Verified in the source group"}).to_string(),
    )
    .unwrap();
    host.call("updateBot", json!({"id":b,"hidden":true})).await;
    let room = host.call("createBot", json!({"kind":"group","name":"Relay","members":[a,b,c]})).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    host.call("send", json!({"botId":room,"text":"@Lead begin relay"})).await;
    host.wait_for(&room, |e| e["kind"] == "agent" && e["data"]["author"] == c && e["data"]["final"] == true).await;
    assert_eq!(
        host.fixture_log("Reviewer", "prompts.jsonl").len(),
        1,
        "key retransmit and display @ do not duplicate work"
    );
    assert_eq!(host.fixture_log("Verifier", "prompts.jsonl").len(), 1);
    let reviewer = host.fixture_log("Reviewer", "prompts.jsonl");
    assert!(reviewer[0].as_str().unwrap().contains("review exact task body"));
    for id in [&a, &b, &c] {
        assert!(finals(&host.call("history", json!({"botId":id})).await).is_empty(), "all results stay in group");
    }
}
