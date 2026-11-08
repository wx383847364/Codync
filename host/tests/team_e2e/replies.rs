//! 回复取回与尝试用量的集成回归，复用父模块的真实 Host 夹具。

use super::*;

/// 群维护在 session/load 后跳过时仍记录未知消耗，正常业务执行不丢失、不重复或归到私聊请求。
#[tokio::test]
async fn group_maintenance_session_recovery_keeps_its_unknown_usage_once() {
    let host = Host::start().await;
    let worker = host.bot("worker").await;
    host.call("updateBot", json!({"id":worker,"manageTeam":false})).await;
    std::fs::write(
        host.home.join("worker/group-script.json"),
        json!({"steps":[
            {"reply":"seeded usage","usage":{"used":70_000,"size":200_000}},
            {"reply":"business completed after session recovery"}
        ]})
        .to_string(),
    )
    .unwrap();
    let room = host.call("createBot", json!({"kind":"group","name":"Usage recovery room","members":[worker]})).await
        ["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let seed = host.call("send", json!({"botId":room,"text":"seed context pressure"})).await;
    group_card(&host, &room, seed["entry"]["id"].as_str().unwrap(), "completed").await;
    std::fs::write(host.home.join("worker/reject-load"), "").unwrap();
    // 工具集变化要求重载原 session，夹具拒绝后走 session/new，维护在 prompt 前退出。
    host.call("updateBot", json!({"id":worker,"manageTeam":true})).await;
    let next = host.call("send", json!({"botId":room,"text":"business survives session recovery"})).await;
    group_card(&host, &room, next["entry"]["id"].as_str().unwrap(), "completed").await;
    let rows: Vec<_> = common::turn_usage(&host.home)
        .into_iter()
        // 仅核验真实执行成员，群卡片本身没有执行器消耗。
        .filter(|row| row.get("bot") == Some(&worker))
        .collect();
    assert_eq!(rows.len(), 3, "two business attempts and one skipped maintenance: {rows:?}");
    // 在实际日志中仅筛选失败尝试，不能把上下文占用或缺失字段当成零消耗。
    let failed: Vec<_> =
        rows.iter().filter(|row| row.get("outcome").is_some_and(|outcome| outcome == "failed")).collect();
    assert_eq!(failed.len(), 1, "maintenance must neither disappear nor be logged twice");
    assert_eq!(failed[0]["scope"], "other");
    assert_eq!(failed[0]["request_ids"], "[]");
    let usage: Value = serde_json::from_str(&failed[0]["usage"]).unwrap();
    assert_eq!(usage["source"], "unavailable");
    assert!(usage["inputTokens"].is_null());
    assert!(usage["outputTokens"].is_null());
    assert!(!host.home.join("worker/compactions.jsonl").exists(), "fresh session must not receive a compact prompt");
    assert_eq!(host.fixture_log("worker", "prompts.jsonl").len(), 2, "accepted business work executes once");
}

/// 验证长回复正文交付与重启后的 MCP 分页、检索和批量范围，不因取证重新执行委派。
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
        format!("结论：已完成\n{}\n中间证据\n{}\n未验证：真实提供方费用", "甲🙂".repeat(2200), "乙".repeat(2200));
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
    host.call(
        "send",
        json!({"botId": lead, "text": format!("READ_REPLY {}", json!({"requestId": request, "query": "中间证据"}))}),
    )
    .await;
    // 检索结果来自真正的 MCP 调用，且不把筛选结果当作完整正文。
    let entry = host
        .wait_for(&lead, |entry| {
            entry["data"]["final"] == true
                && entry["data"]["text"].as_str().is_some_and(|text| text.contains("\"matches\""))
        })
        .await;
    let matched: Value = serde_json::from_str(entry["data"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(matched["matches"][0]["text"], "中间证据\n");
    let offset = "结论：已完成\n".chars().count() + 4401;
    host.call("send", json!({"botId": lead, "text": format!("READ_REPLY {}", json!({"requestId": request, "ranges": [{"offset": 0, "limit": 2}, {"offset": offset, "limit": 4}]}))})).await;
    // 同一次模型可见返回包含两个分离范围，不传递其间的填充正文。
    let entry = host
        .wait_for(&lead, |entry| {
            entry["data"]["final"] == true
                && entry["data"]["text"].as_str().is_some_and(|text| text.contains("\"slices\""))
        })
        .await;
    let batched: Value = serde_json::from_str(entry["data"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(batched["slices"][0]["text"], "结论");
    assert_eq!(batched["slices"][1]["text"], "中间证据");
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
