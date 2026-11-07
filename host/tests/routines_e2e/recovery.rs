use super::*;

/// 无法恢复的隐藏回合留下未知观测，之后正常回合不能覆盖或重复它。
#[tokio::test]
async fn unavailable_resume_never_replays_work_in_a_fresh_session() {
    let mut host = Host::start().await;
    let bot = host.stable_bot("no-resume").await;
    let id = host.save(&bot, "unloadable", "BLOCK[original]", json!([{"type":"webhook"}]), 30).await;
    host.call("runRoutine", json!({"botId":bot,"id":id})).await;
    host.wait_event("no-resume", "started", "BLOCK[original]").await;
    std::fs::write(host.home.join("no-resume/no-load"), "").unwrap();
    host.restart().await;
    host.wait_status(&bot, &id, "interrupted").await;
    let next = host.save(&bot, "next", "AFTER_INTERRUPTION", json!([{"type":"webhook"}]), 30).await;
    host.call("runRoutine", json!({"botId":bot,"id":next})).await;
    host.wait_status(&bot, &next, "succeeded").await;
    assert_eq!(
        host.agent_events("no-resume")
            .iter()
            .filter(|e| e["text"].as_str().unwrap().contains("BLOCK[original]"))
            .count(),
        1
    );
    let rows: Vec<_> = common::turn_usage(&host.home).into_iter().filter(|r| r.get("bot") == Some(&bot)).collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["outcome"], "incomplete");
    assert!(rows[0]["usage"].contains("unavailable"));
    assert_eq!(rows[1]["outcome"], "completed");
    assert_ne!(rows[0]["attempt"], rows[1]["attempt"]);
}
