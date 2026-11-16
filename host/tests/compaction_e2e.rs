//! Compaction and rollover on a real host with disposable data: the host compacting a full conversation at the
//! end of a turn, its loop guard turning to a rollover, and the handoff card the user edits or turns down.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Waits until the bot's chat has a notice with this field, and returns the newest one.
async fn notice(host: &Host, bot: &str, field: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let history = host.call("history", json!({"botId": bot})).await;
        if let Some(e) = history["entries"].as_array().unwrap().iter().rev().find(|e| e["data"][field].is_object()) {
            return e.clone();
        }
        assert!(Instant::now() < deadline, "no {field} notice: {history}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_full_conversation_is_compacted_with_its_keep_list() {
    let host = Host::start().await;
    let claude = host.fixture_bot("Claude-like", "instructions_agent.py", "claude").await;
    host.reply(&claude, "hello").await;
    host.write_memory(Some(&claude), "MEMORY.md", "- Allergic to peanuts\n");
    host.reply(&claude, "USAGE 210000 SIZE 1000000").await;
    let line = notice(&host, &claude, "compaction").await;
    assert_eq!(line["data"]["compaction"]["byHost"], true);
    assert_eq!(line["data"]["compaction"]["summary"], "Summary 1: the user is planning a trip.");
    let prompts = host.prompts(&claude);
    let compact = prompts.iter().find(|p| p.starts_with("/compact")).unwrap();
    assert!(compact.starts_with("/compact When your context is summarized"), "Claude gets the keep-list: {compact}");
    // Memory changed since the session opened, so the compaction keeps an opening rendered just before it.
    let loads = host.requests(&claude, "session/load");
    let opening = loads.last().unwrap()["params"]["_meta"]["systemPrompt"]["append"].as_str().unwrap();
    assert!(opening.contains("Allergic to peanuts"), "{opening}");

    let codex = host.fixture_bot("Codex-like", "instructions_agent.py", "codex").await;
    host.reply(&codex, "USAGE 150000 SIZE 200000").await;
    notice(&host, &codex, "compaction").await;
    assert!(host.prompts(&codex).iter().any(|p| p == "/compact"), "Codex's keep-list is in its config");
}

#[tokio::test]
async fn compacting_too_often_turns_into_a_rollover_the_user_can_edit_or_turn_down() {
    let host = Host::start().await;
    let bot = host.fixture_bot("Busy", "instructions_agent.py", "codex").await;
    host.reply(&bot, "USAGE 210000 SIZE 1000000").await;
    notice(&host, &bot, "compaction").await;
    // Full again right after a compaction: compacting again wouldn't help.
    host.reply(&bot, "USAGE 210000 SIZE 1000000").await;
    let card = notice(&host, &bot, "handoff").await;
    assert_eq!(card["data"]["handoff"]["status"], "draft");
    let text = card["data"]["handoff"]["text"].as_str().unwrap();
    assert!(text.starts_with("<handoff>") && text.contains("USAGE 210000"), "{text}");

    host.call("editHandoff", json!({"entryId": card["id"], "text": "<handoff>We were booking a train.</handoff>"}))
        .await;
    host.reply(&bot, "Which seat did we pick?").await;
    let news = host.requests(&bot, "session/new");
    assert_eq!(news.len(), 2, "a new conversation");
    let last = host.prompts(&bot).last().unwrap().clone();
    assert!(last.contains("We were booking a train.") && last.contains("Which seat did we pick?"), "{last}");
    let card = host.call("history", json!({"botId": bot})).await["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == card["id"])
        .unwrap()
        .clone();
    assert_eq!(card["data"]["handoff"]["status"], "sent");
    assert!(host.refused("editHandoff", json!({"entryId": card["id"], "text": "late"})).await.contains("moved on"));

    // Turned down: the conversation carries on.
    host.reply(&bot, "USAGE 210000 SIZE 1000000").await;
    notice(&host, &bot, "compaction").await;
    host.reply(&bot, "USAGE 210000 SIZE 1000000").await;
    let card = host.call("history", json!({"botId": bot})).await["entries"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["data"]["handoff"]["status"] == "draft")
        .unwrap()
        .clone();
    host.call("keepConversation", json!({"entryId": card["id"]})).await;
    host.reply(&bot, "carry on").await;
    assert_eq!(host.requests(&bot, "session/new").len(), 2, "same conversation");
    assert!(!host.prompts(&bot).last().unwrap().contains("<handoff>"));
}

/// 真实 Host 应保存实报用量并跨重启保留待确认消息，用编辑后的摘要在同一机器人继续任务。
#[tokio::test]
async fn capacity_handoff_survives_restart_and_runs_the_queued_task_once() {
    let mut host = Host::start().await;
    let bot = host.fixture_bot("Long-running", "instructions_agent.py", "first-resume").await;
    assert_eq!(host.call("botSessionUsage", json!({"botId": bot})).await["hasSession"], false);
    host.reply(&bot, "USAGE 12000 SIZE 18000 REPORT_TOKENS PAUSE_NEXT_SESSION").await;
    let deadline = Instant::now() + Duration::from_secs(5);
    let usage = loop {
        let usage = host.call("botSessionUsage", json!({"botId": bot})).await;
        if !usage["attempts"].as_array().unwrap().is_empty() {
            break usage;
        }
        assert!(Instant::now() < deadline, "missing recorded attempt: {usage}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(usage["used"], 12000);
    assert_eq!(usage["size"], 18000);
    assert_eq!(usage["attempts"][0]["rows"][0]["value"], 1234);
    assert_eq!(usage["attempts"][0]["rows"][1]["value"], 0);
    assert!(usage["attempts"][0]["rows"][2]["value"].is_null());

    let task =
        format!("next work item {}", "provide a clear response, include references, and review changes. ".repeat(550));
    let entry = host.call("send", json!({"botId": bot, "text":task})).await["entry"]["id"].clone();
    let card = notice(&host, &bot, "handoff").await;
    assert_eq!(card["data"]["handoff"]["reason"], "capacity");
    assert!(!card["data"]["handoff"]["text"].as_str().unwrap().contains("next work item"));
    assert_eq!(host.prompts(&bot).len(), 1, "pressure waits for the user");
    host.restart(|_| {}).await;
    let history = host.call("history", json!({"botId": bot})).await;
    assert!(history["entries"].as_array().unwrap().iter().any(|e| e["id"] == entry && e["data"]["status"] == "queued"));
    host.call(
        "editHandoff",
        json!({"entryId": card["id"], "text": "<handoff>Preserve the acceptance criteria.</handoff>"}),
    )
    .await;
    // 最旧客户端把 /new 当消息发送，同样能确认卡片。
    host.call("send", json!({"botId": bot, "text": "/new"})).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.requests(&bot, "session/new").len() < 2 {
        assert!(Instant::now() < deadline, "the accepted handoff did not try to open a session");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(host.prompts(&bot).len(), 1, "the new session has not sent the task yet");
    host.crash();
    host.restart(|_| {}).await;
    host.call("startHandoff", json!({"entryId": card["id"]})).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let history = host.call("history", json!({"botId": bot})).await;
        let entries = history["entries"].as_array().unwrap();
        if entries.iter().filter(|e| e["kind"] == "agent" && e["data"]["final"] == true).count() == 2 {
            assert!(entries.iter().any(|e| e["id"] == entry && e["data"]["status"] == "sent"));
            assert!(entries.iter().any(|e| e["id"] == card["id"] && e["data"]["handoff"]["status"] == "sent"));
            break;
        }
        assert!(Instant::now() < deadline, "the confirmed work did not run: {history}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(host.requests(&bot, "session/new").len(), 3);
    let usage = host.call("botSessionUsage", json!({"botId": bot})).await;
    assert_eq!(usage["hasSession"], true);
    assert!(usage["used"].is_null(), "a new process can reuse a session id");
    assert!(usage["size"].is_null());
    let prompts = host.prompts(&bot);
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("Preserve the acceptance criteria."));
    assert!(prompts[1].contains(task.trim_end()));
}
