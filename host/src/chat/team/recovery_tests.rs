use super::*;

/// 后发请求先完成时，重启保持回复的完成顺序。
#[tokio::test]
async fn reply_recovery_preserves_completion_order() {
    let f = Fixture::new();
    f.send_to("a", "BLOCK sender").await;
    f.until(|| !f.prompts_of("a").is_empty()).await;
    let slow = f.message("BLOCK slow reply").unwrap();
    f.until(|| f.prompts().len() == 1).await;
    let fast = call(&f.hub, "a", "message_bot", &json!({"botId":"c","message":"fast reply"})).unwrap();
    f.until(|| f.hub.store.pending_local_replies().unwrap().len() == 1).await;
    std::fs::write(f.dir.join("b/release"), "").unwrap();
    f.until(|| f.hub.store.pending_local_replies().unwrap().len() == 2).await;
    let f = f.restart().await;
    f.until(|| f.heard(&slow) && f.heard(&fast) && f.idle("a")).await;
    let prompts = f.prompts_of("a").join("\n");
    assert!(
        prompts.find(fast["requestId"].as_str().unwrap()).unwrap()
            < prompts.find(slow["requestId"].as_str().unwrap()).unwrap()
    );
    f.shutdown().await;
}

/// 消费事务失败时不写 ACP，下一次启动仍可投递此前保存的回复。
#[tokio::test]
async fn failed_reply_handoff_does_not_send_the_prompt() {
    let f = Fixture::new();
    let db = rusqlite::Connection::open(f.dir.join("test.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_reply_handoff BEFORE DELETE ON kv
         WHEN OLD.k LIKE 'team.pendingReply.%'
         BEGIN SELECT RAISE(ABORT, 'fixture send boundary failure'); END;",
    )
    .unwrap();
    let receipt = f.message("pending reply").unwrap();
    f.until(|| f.hub.runtime("a").status == BotStatus::Error).await;
    assert_eq!(f.prompts_of("a").len(), 0);
    assert_eq!(f.hub.store.pending_local_replies().unwrap().len(), 1);
    db.execute_batch("DROP TRIGGER reject_reply_handoff;").unwrap();
    drop(db);
    let f = f.restart().await;
    f.until(|| f.heard(&receipt) && f.idle("a")).await;
    assert_eq!(f.prompts_of("a").len(), 1);
    assert_eq!(f.hub.store.pending_local_replies().unwrap().len(), 0);
    f.shutdown().await;
}

/// 删除忙碌发送方时当场删除待送正文，不依赖下次启动时清扫。
#[tokio::test]
async fn deleting_a_sender_removes_its_pending_reply_immediately() {
    let f = Fixture::new();
    f.send_to("a", "BLOCK sender").await;
    f.until(|| !f.prompts_of("a").is_empty()).await;
    f.message("erase this reply with the sender").unwrap();
    f.until(|| f.hub.store.kv_prefix("team.pendingReply.").len() == 1).await;
    f.hub.delete_bot("a").unwrap();
    assert_eq!(f.hub.store.kv_prefix("team.pendingReply.").len(), 0);
    f.shutdown().await;
}

/// 调度重启只恢复明确未开始的本机请求；已开始请求仅回传中断。
#[tokio::test]
async fn restart_restores_only_requests_that_had_not_started() {
    let f = Fixture::new();
    let running = f.message("BLOCK active message").unwrap();
    f.until(|| f.prompts().len() == 1).await;
    let queued = f.message("queued message").unwrap();
    assert!(f.hub.store.kv_get("turn.inflight.b").is_none_or(|v| v.is_empty()));
    let f = f.restart().await;
    f.until(|| f.heard(&running) && f.heard(&queued) && f.idle("a")).await;
    assert_eq!(f.prompts().len(), 2);
    assert!(f.prompts()[1].ends_with("\n\nqueued message"));
    assert!(f.notices(&running).iter().all(|entry| entry.data["status"] == "failed"));
    assert!(f.notices(&queued).iter().all(|entry| entry.data["status"] == "completed"));
    assert!(f.prompts_of("a").join("\n").contains("Interrupted by host restart"));
    assert_eq!(f.hub.team.0.locked().messages, 0);
    f.shutdown().await;
}

/// 忙碌发送方跨重启收到超出通知预览的正文，第二次重启不再次派送或执行。
#[tokio::test]
async fn pending_long_reply_survives_restart_and_is_consumed() {
    let f = Fixture::new();
    let receipt = f.message("BLOCK worker").unwrap();
    f.until(|| f.prompts().len() == 1).await;
    let long = f.message("LONG_REPLY").unwrap();
    f.send_to("a", "BLOCK sender").await;
    f.until(|| !f.prompts_of("a").is_empty()).await;
    std::fs::write(f.dir.join("b/release"), "").unwrap();
    f.until(|| f.hub.store.pending_local_replies().unwrap().len() == 2).await;
    let f = f.restart().await;
    f.until(|| f.heard(&receipt) && f.heard(&long) && f.idle("a")).await;
    assert!(f.prompts_of("a").join("\n").contains("中间证据"));
    assert!(f.prompts_of("a").join("\n").contains("未验证：真实提供方费用"));
    assert_eq!(f.hub.store.pending_local_replies().unwrap().len(), 0);
    let before = f.prompts_of("a");
    let f = f.restart().await;
    f.send_to("a", "after restart").await;
    f.until(|| f.took("a", before.len() + 1)).await;
    assert_eq!(f.prompts_of("a")[..before.len()], before);
    assert!(!f.prompts_of("a").last().unwrap().contains(long["requestId"].as_str().unwrap()));
    assert_eq!(f.prompts().len(), 2);
    f.shutdown().await;
}
