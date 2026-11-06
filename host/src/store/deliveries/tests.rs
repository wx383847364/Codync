use super::*;
use serde_json::json;

/// 使用同库真实事务验证投递，不依赖 actor 内存队列。
fn store() -> Store {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    for id in ["a", "b", "c"] {
        store
            .save_bot(&serde_json::from_value(json!({"id":id,"name":id,"backend":"claude","cwd":"/tmp"})).unwrap())
            .unwrap();
    }
    store
        .save_bot(
            &serde_json::from_value(json!({"id":"g","name":"Crew","kind":"group","members":["a","b","c"]})).unwrap(),
        )
        .unwrap();
    store
}

/// started 是重启后不自动重放的边界，claimed 则安全回队。
#[test]
fn recovery_preserves_queued_and_interrupts_started() {
    let s = store();
    let (_, _) = s.post_group("g", &json!({"text":"work"}), &["a".into(), "b".into(), "c".into()]).unwrap();
    let rows = s.queued_deliveries("g").unwrap();
    s.claim_delivery(&rows[0].id, "a").unwrap();
    s.claim_delivery(&rows[1].id, "b").unwrap();
    s.start_delivery(&rows[1].id, "b").unwrap();
    s.recover_deliveries().unwrap();
    assert_eq!(s.delivery(&rows[0].id).unwrap().unwrap().status, Status::Queued);
    assert_eq!(s.delivery(&rows[1].id).unwrap().unwrap().status, Status::Interrupted);
    assert_eq!(s.queued_deliveries("g").unwrap().len(), 2);
}

/// 幂等重传优先于满额判断，同键不同正文不会读出另一项任务。
#[test]
fn idempotency_precedes_capacity_and_conflicts_are_explicit() {
    let s = store();
    let mut first = None;
    for i in 0..16 {
        let mut d = Delivery::speak("g", "unused", "b");
        d.kind = Kind::Task;
        d.source_kind = "tool".into();
        d.source_entry_id = None;
        d.source_bot_id = Some("a".into());
        d.source_execution_id = None;
        d.idempotency_key = Some(format!("key{i}"));
        d.body = Some(format!("body{i}"));
        let (saved, _) = s.enqueue_task(&d).unwrap();
        if i == 0 {
            first = Some(saved);
        }
    }
    let first = first.unwrap();
    assert_eq!(s.enqueue_task(&first).unwrap().0.id, first.id);
    let mut changed = first.clone();
    changed.body = Some("different".into());
    assert!(s.enqueue_task(&changed).unwrap_err().to_string().contains("幂等键"));
    changed.idempotency_key = None;
    assert!(s.enqueue_task(&changed).unwrap_err().to_string().contains("容量"));
}

/// 领取绑定指定 ID 与目标，取消后的完成不复活、不派生新任务。
#[test]
fn claim_and_late_completion_are_conditional() {
    let s = store();
    s.post_group("g", &json!({"text":"work"}), &["a".into()]).unwrap();
    let d = s.queued_deliveries("g").unwrap().remove(0);
    assert!(s.claim_delivery(&d.id, "b").unwrap().0.is_none());
    assert!(s.claim_delivery(&d.id, "a").unwrap().0.is_some());
    assert!(s.claim_delivery(&d.id, "a").unwrap().0.is_none());
    s.start_delivery(&d.id, "a").unwrap();
    s.cancel_deliveries("g", None, "stop").unwrap();
    let entries = s.complete_delivery(&d.id, Status::Completed, Some("@b do it"), None, 1).unwrap();
    assert_eq!(s.delivery(&d.id).unwrap().unwrap().status, Status::Cancelled);
    assert!(s.queued_deliveries("g").unwrap().is_empty());
    assert!(entries.iter().filter(|e| e.kind == "agent").all(|e| e.data["final"] == false));
}

/// 九个预占后并发领取两项，只能成功一次；剩余queued被显式取消。
#[test]
fn concurrent_claim_reserves_the_tenth_budget_atomically() {
    let s = std::sync::Arc::new(store());
    let mut ids = Vec::new();
    for i in 0..12 {
        let mut d = Delivery::speak("g", &format!("source{i}"), "a");
        d.batch_root_id = Some("root".into());
        insert(&s.db.locked(), &d).unwrap();
        ids.push(d.id);
    }
    for id in &ids[..9] {
        assert!(s.claim_delivery(id, "a").unwrap().0.is_some());
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads = ids[9..11]
        .iter()
        .map(|id| {
            let (s, id, barrier) = (s.clone(), id.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                s.claim_delivery(&id, "a").unwrap().0.is_some()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(threads.into_iter().map(|t| usize::from(t.join().unwrap())).sum::<usize>(), 1);
    let count: i64 =
        s.db.locked().query_row("SELECT COUNT(*) FROM deliveries WHERE status='claimed'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 10);
    assert!(s.queued_deliveries("g").unwrap().is_empty());
}

/// 多发送者同时入队时全局不超过64，同一发送者不超过16。
#[test]
fn concurrent_capacity_is_bounded_across_senders_and_retransmissions() {
    let s = std::sync::Arc::new(store());
    for id in ["d", "e"] {
        s.save_bot(&serde_json::from_value(json!({"id":id,"name":id})).unwrap()).unwrap();
    }
    let mut group = s.bot("g").unwrap().unwrap().config;
    group.members = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
    s.save_bot(&group).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
    let handles = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|source| {
            let (s, barrier) = (s.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                let mut accepted = 0;
                for i in 0..20 {
                    let mut d = Delivery::speak("g", "unused", if source == "b" { "c" } else { "b" });
                    d.kind = Kind::Task;
                    d.source_kind = "tool".into();
                    d.source_entry_id = None;
                    d.source_bot_id = Some(source.into());
                    d.body = Some(format!("{source}-{i}"));
                    if s.enqueue_task(&d).is_ok() {
                        accepted += 1;
                    }
                }
                assert!(accepted <= 16);
                accepted
            })
        })
        .collect::<Vec<_>>();
    let accepted = handles.into_iter().map(|h| h.join().unwrap()).sum::<usize>();
    assert_eq!(accepted, 64);
    assert_eq!(s.queued_deliveries("g").unwrap().len(), 64);
}

/// 取消后的来源不能再派生新任务；此前已接受键的重传仍返回原记录。
#[test]
fn stopped_source_cannot_create_new_work_but_keyed_retransmit_is_stable() {
    let s = store();
    let (e, _) = s.post_group("g", &json!({"text":"source"}), &["a".into()]).unwrap();
    let source = s.queued_deliveries("g").unwrap().remove(0);
    s.claim_delivery(&source.id, "a").unwrap();
    s.start_delivery(&source.id, "a").unwrap();
    let mut d = Delivery::speak("g", &e.id, "b");
    d.kind = Kind::Task;
    d.source_kind = "tool".into();
    d.source_entry_id = None;
    d.source_execution_id = Some(source.id);
    d.source_bot_id = Some("a".into());
    d.body = Some("exact".into());
    d.idempotency_key = Some("stable".into());
    let original = s.enqueue_task(&d).unwrap().0;
    s.cancel_deliveries("g", None, "stop").unwrap();
    assert_eq!(s.enqueue_task(&d).unwrap().0.id, original.id);
    d.idempotency_key = Some("new".into());
    assert!(s.enqueue_task(&d).is_err());
}

/// 仅在单元测试构建中注入SQLite只读故障，不给产品新增故障配置开关。
impl Store {
    pub(crate) fn delivery_test_readonly(&self, readonly: bool) {
        self.db.locked().pragma_update(None, "query_only", readonly).unwrap();
    }
}

/// 子进程在四个持久提交边界停住，父进程发强杀而非析构Hub。
#[test]
#[ignore = "only invoked by the crash-boundary parent"]
fn delivery_crash_worker() {
    let dir = std::env::var("CODYNC_DELIVERY_CRASH_DIR").unwrap();
    let phase = std::env::var("CODYNC_DELIVERY_CRASH_PHASE").unwrap();
    let s = Store::open(&std::path::Path::new(&dir).join("crash.db")).unwrap();
    s.save_bot(&serde_json::from_value(json!({"id":"a","name":"a"})).unwrap()).unwrap();
    s.save_bot(&serde_json::from_value(json!({"id":"g","name":"g","kind":"group","members":["a"]})).unwrap()).unwrap();
    s.post_group("g", &json!({"text":"complete original"}), &["a".into()]).unwrap();
    let d = s.queued_deliveries("g").unwrap().remove(0);
    if phase != "queued" {
        s.claim_delivery(&d.id, "a").unwrap();
    }
    if matches!(phase.as_str(), "started" | "completed") {
        s.start_delivery(&d.id, "a").unwrap();
    }
    if phase == "completed" {
        s.complete_delivery(&d.id, Status::Completed, Some("saved result"), None, 1).unwrap();
    }
    std::fs::write(std::path::Path::new(&dir).join("ready"), &d.id).unwrap();
    loop {
        std::thread::park();
    }
}

/// 入队、领取、提交启动和完成保存后强杀；真实发送强杀由 `team_e2e` 覆盖。
#[test]
fn subprocess_crashes_obey_persistent_recovery_boundaries() {
    for phase in ["queued", "claimed", "started", "completed"] {
        let dir = std::env::temp_dir().join(format!("codync-delivery-crash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "store::deliveries::tests::delivery_crash_worker", "--ignored"])
            .env("CODYNC_DELIVERY_CRASH_DIR", &dir)
            .env("CODYNC_DELIVERY_CRASH_PHASE", phase)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !dir.join("ready").exists() && std::time::Instant::now() < deadline && child.try_wait().unwrap().is_none()
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let ready = std::fs::read_to_string(dir.join("ready"));
        let _ = child.kill();
        let _ = child.wait();
        let id = ready.expect("crash worker reached commit boundary");
        let s = Store::open(&dir.join("crash.db")).unwrap();
        s.recover_deliveries().unwrap();
        let expected = match phase {
            "queued" | "claimed" => Status::Queued,
            "started" => Status::Interrupted,
            _ => Status::Completed,
        };
        assert_eq!(s.delivery(&id).unwrap().unwrap().status, expected);
        if phase == "completed" {
            assert_eq!(s.entry(&format!("delivery-result-{id}")).unwrap().data["text"], "saved result");
        }
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// 删除成员的取消与存活群卡片同时落盘，返回的发布数据也必须反映终态。
#[test]
fn deleting_a_member_commits_cancelled_status_cards_for_live_rooms() {
    let s = store();
    let (source, _) = s.post_group("g", &json!({"text":"both members"}), &["a".into(), "b".into()]).unwrap();
    let queued = s.queued_deliveries("g").unwrap();
    let removed = queued.iter().find(|d| d.target_bot_id == "a").unwrap();
    let (_, cancelled, cards) = s.delete_bot("a").unwrap();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].id, removed.id);
    assert_eq!(s.delivery(&removed.id).unwrap().unwrap().status, Status::Cancelled);
    let id = format!("delivery-card-{}-1", source.id);
    let card = s.entry(&id).unwrap();
    assert_eq!(card.data["deliveryRows"][0]["status"], "cancelled");
    assert_eq!(card.data["deliveryRows"][1]["status"], "queued");
    assert!(cards.iter().any(|entry| entry.id == id && entry.data == card.data));
    s.recover_deliveries().unwrap();
    assert_eq!(s.entry(&id).unwrap().data, card.data, "reconnection does not need an actor to repair the card");
}

/// 成员更新中途写库失败必须回滚配置，重试后才一起提交取消卡与终态。
#[test]
fn member_removal_rolls_back_with_failed_delivery_cancellation() {
    let s = store();
    let (source, _) = s.post_group("g", &json!({"text":"@a task"}), &["a".into()]).unwrap();
    let delivery = s.queued_deliveries("g").unwrap().remove(0);
    s.claim_delivery(&delivery.id, "a").unwrap();
    s.start_delivery(&delivery.id, "a").unwrap();
    let mut group = s.bot("g").unwrap().unwrap().config;
    group.members.retain(|id| id != "a");
    s.db.locked().execute_batch("CREATE TEMP TRIGGER reject_cancel BEFORE UPDATE OF status ON deliveries WHEN NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'simulated cancellation failure'); END;").unwrap();
    assert!(s.update_bot(&group).is_err());
    assert!(s.bot("g").unwrap().unwrap().config.members.contains(&"a".into()));
    assert_eq!(s.delivery(&delivery.id).unwrap().unwrap().status, Status::Started);
    s.db.locked().execute_batch("DROP TRIGGER reject_cancel").unwrap();
    let (cancelled, cards) = s.update_bot(&group).unwrap();
    assert_eq!(cancelled[0].id, delivery.id);
    assert_eq!(cancelled[0].status, Status::Cancelled);
    assert_eq!(cards[0].data["deliveryRows"][0]["status"], "cancelled");
    assert!(!s.bot("g").unwrap().unwrap().config.members.contains(&"a".into()));
    assert_eq!(
        s.entry(&format!("delivery-card-{}-1", source.id)).unwrap().data["deliveryRows"][0]["status"],
        "cancelled"
    );
}

/// 活跃群语音的原子结果携带通话身份；通话结束后的后续结果恢复普通群消息。
#[test]
fn delivery_results_keep_only_the_still_open_source_call() {
    let s = store();
    let (_, _) =
        s.post_group("g", &json!({"text":"voice question", "call":"voice-1"}), &["a".into(), "b".into()]).unwrap();
    let deliveries = s.queued_deliveries("g").unwrap();
    for d in &deliveries {
        s.claim_delivery(&d.id, &d.target_bot_id).unwrap();
        s.start_delivery(&d.id, &d.target_bot_id).unwrap();
    }
    let entries = s.complete_delivery(&deliveries[0].id, Status::Completed, Some("spoken reply"), None, 1).unwrap();
    assert_eq!(entries.iter().find(|entry| entry.kind == "agent").unwrap().data["call"], "voice-1");
    s.kv_set("call.ended.voice-1", "1").unwrap();
    let entries = s.complete_delivery(&deliveries[1].id, Status::Completed, Some("after call"), None, 1).unwrap();
    assert!(entries.iter().find(|entry| entry.kind == "agent").unwrap().data["call"].is_null());
}

/// 同键并发重传只创建一行，异文冲突不能改变已接受的原文。
#[test]
fn concurrent_keyed_retransmits_share_one_stable_delivery_and_reject_conflicts() {
    let s = std::sync::Arc::new(store());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let (s, barrier) = (s.clone(), barrier.clone());
            // 模拟多个连接同时重发同一工具请求，各自生成不同的候选ID。
            std::thread::spawn(move || {
                let mut d = Delivery::speak("g", "unused", "b");
                d.kind = Kind::Task;
                d.source_kind = "tool".into();
                d.source_entry_id = None;
                d.source_bot_id = Some("a".into());
                d.body = Some("exact original".into());
                d.idempotency_key = Some("shared-key".into());
                barrier.wait();
                let accepted = s.enqueue_task(&d).unwrap().0;
                d.body = Some("changed body".into());
                assert!(s.enqueue_task(&d).unwrap_err().to_string().contains("幂等键已用于另一个任务"));
                accepted.id
            })
        })
        .collect::<Vec<_>>();
    let ids = handles.into_iter().map(|h| h.join().unwrap()).collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 1);
    let queued = s.queued_deliveries("g").unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].body.as_deref(), Some("exact original"));
}
