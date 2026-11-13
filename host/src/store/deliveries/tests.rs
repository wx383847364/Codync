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

/// 把现有测试群设为阶段群，默认负责人选第二位以区分名单顺序与默认路由。
fn stage_store() -> Store {
    let s = store();
    let mut group = s.bot("g").unwrap().unwrap().config;
    group.stage = Some(crate::store::StageGroup {
        leader_id: "b".into(),
        goal: "Build a fixture".into(),
        acceptance: "Verify output".into(),
        ..Default::default()
    });
    s.save_bot(&group).unwrap();
    s
}

/// 负责人失效时保留重发机会，显式路由可继续工作，重复 nonce 沿用原接收人。
#[test]
fn stage_default_recipient_is_saved_with_the_message() {
    let s = stage_store();
    let body = json!({"text":"work", "clientNonce":"stable"});
    let (saved, _) = s.post_group("g", &body, &[]).unwrap();
    assert_eq!(saved.data["resolvedMentions"], json!(["b"]));
    assert_eq!(s.queued_deliveries("g").unwrap()[0].target_bot_id, "b");
    let directed = json!({"text":"see this", "replyTo":{"author":"removed"}});
    assert!(s.post_group("g", &directed, &[]).unwrap_err().to_string().contains("quoted member"));
    assert_eq!(s.post_group("g", &directed, &["a".into()]).unwrap().0.data["resolvedMentions"], json!(["a"]));
    s.delete_bot("b").unwrap();
    assert_eq!(s.post_group("g", &body, &[]).unwrap().0.id, saved.id);
    let before = s.history("g", i64::MAX, 20).unwrap().len();
    assert!(s.post_group("g", &json!({"text":"invalid"}), &[]).is_err());
    assert_eq!(s.history("g", i64::MAX, 20).unwrap().len(), before);
    let (direct, _) = s.post_group("g", &json!({"text":"@a work"}), &["a".into()]).unwrap();
    assert_eq!(direct.data["resolvedMentions"], json!(["a"]));
}

/// 无负责人阶段群默认询问所有成员，显式提及优先，失效引用不会广播。
#[test]
fn leaderless_stage_defaults_to_members_unless_addressed() {
    let s = stage_store();
    let mut group = s.bot("g").unwrap().unwrap().config;
    group.stage.as_mut().unwrap().leader_id.clear();
    s.save_bot(&group).unwrap();
    let body = json!({"text":"review the result"});
    assert_eq!(s.post_group("g", &body, &[]).unwrap().0.data["resolvedMentions"], json!(["a", "b", "c"]));
    assert_eq!(s.post_group("g", &body, &["b".into()]).unwrap().0.data["resolvedMentions"], json!(["b"]));
    let reply = json!({"text":"review the result","replyTo":{"author":"removed"}});
    assert!(s.post_group("g", &reply, &[]).unwrap_err().to_string().contains("quoted member"));
    assert_eq!(s.post_group("g", &reply, &["b".into()]).unwrap().0.data["resolvedMentions"], json!(["b"]));
}

/// 普通群有负责人时默认定向，移除负责人后恢复按成员顺序询问。
#[test]
fn ordinary_group_default_recipient_yields_to_explicit_mentions() {
    let s = store();
    let mut cfg = s.bot("g").unwrap().unwrap().config;
    cfg.group_leader_id = Some("b".into());
    s.save_bot(&cfg).unwrap();
    let body = json!({"text":"review the result"});
    assert_eq!(s.post_group("g", &body, &[]).unwrap().0.data["resolvedMentions"], json!(["b"]));
    assert_eq!(s.post_group("g", &body, &["c".into()]).unwrap().0.data["resolvedMentions"], json!(["c"]));
    assert_eq!(
        s.post_group("g", &body, &["a".into(), "b".into(), "c".into()]).unwrap().0.data["resolvedMentions"],
        json!(["a", "b", "c"])
    );
    s.delete_bot("b").unwrap();
    assert_eq!(s.post_group("g", &body, &[]).unwrap().0.data["resolvedMentions"], json!(["a", "c"]));
}

/// 显式请求的结果与文字 @ 共用一次继续处理，未请求及无结果的派工只更新状态。
#[test]
fn stage_returns_only_requested_final_results_once() {
    for (source_kind, status, text, file, expected) in [
        (TOOL_RETURN, Status::Completed, Some("result"), false, 1),
        (TOOL_RETURN, Status::Completed, Some("@b result"), false, 1),
        (TOOL_RETURN, Status::Completed, None, true, 1),
        ("tool", Status::Completed, Some("result"), false, 0),
        (TOOL_RETURN, Status::NoReply, None, false, 0),
    ] {
        let s = stage_store();
        let mut request = Delivery::speak("g", "unused", "a");
        request.kind = Kind::Task;
        request.source_kind = source_kind.into();
        request.source_entry_id = None;
        request.source_bot_id = Some("b".into());
        request.body = Some("review".into());
        request.idempotency_key = Some("review-1".into());
        let delivery = s.enqueue_task(&request).unwrap().0;
        let mut conflict = request;
        conflict.source_kind = if source_kind == "tool" { TOOL_RETURN } else { "tool" }.into();
        assert!(s.enqueue_task(&conflict).is_err());
        s.claim_delivery(&delivery.id, "a").unwrap();
        s.start_delivery(&delivery.id, "a").unwrap();
        let source = if file {
            s.insert_entry(
                &crate::store::Lane::main("g"),
                EntryKind::Agent,
                1,
                &json!({"deliveryId":delivery.id,"author":"a","final":true,"text":"@c report.md",
                    "files":[{"id":"snapshot","name":"@c report.md"}]}),
            )
            .unwrap()
            .id
        } else {
            format!("delivery-result-{}", delivery.id)
        };
        s.complete_delivery(&delivery.id, status, text, None, 1).unwrap();
        s.complete_delivery(&delivery.id, status, text, None, 1).unwrap();
        let queued = s.queued_deliveries("g").unwrap();
        assert_eq!(queued.len(), expected);
        if let Some(next) = queued.first() {
            assert_eq!(next.target_bot_id, "b");
            assert_eq!(next.source_execution_id.as_deref(), Some(delivery.id.as_str()));
            assert_eq!(next.source_entry_id.as_deref(), Some(source.as_str()));
        }
    }
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

/// 同一批次超过十次仍可继续领取；两个不同 ID 的并发领取均保留。
#[test]
fn concurrent_claims_continue_past_ten_without_cancelling_accepted_work() {
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
    assert_eq!(threads.into_iter().map(|t| usize::from(t.join().unwrap())).sum::<usize>(), 2);
    let count: i64 =
        s.db.locked().query_row("SELECT COUNT(*) FROM deliveries WHERE status='claimed'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 11);
    assert_eq!(s.queued_deliveries("g").unwrap().len(), 1);
    assert!(s.claim_delivery(&ids[11], "a").unwrap().0.is_some());
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

/// 文字与工具混合超过旧的轮次、发言和深度上限，直至明确交付给用户才自然空闲。
#[test]
fn mixed_group_handoffs_continue_until_the_result_is_handed_to_the_user() {
    let s = store();
    s.post_group("g", &json!({"text":"完成实现与复核后交付"}), &["a".into()]).unwrap();
    let mut current = s.queued_deliveries("g").unwrap().remove(0);
    let root = current.batch_root_id.clone();
    for step in 0..25 {
        assert!(s.claim_delivery(&current.id, &current.target_bot_id).unwrap().0.is_some());
        assert!(s.start_delivery(&current.id, &current.target_bot_id).unwrap().0);
        let target = if current.target_bot_id == "a" { "b" } else { "a" };
        if step == 24 {
            s.complete_delivery(
                &current.id,
                Status::Completed,
                Some("交付：实现与复核完成，请用户确认结果。"),
                None,
                1,
            )
            .unwrap();
            break;
        }
        if step % 2 == 1 {
            let mut task = Delivery::speak("g", "unused", target);
            task.kind = Kind::Task;
            task.source_kind = "tool".into();
            task.source_entry_id = None;
            task.source_execution_id = Some(current.id.clone());
            task.source_bot_id = Some(current.target_bot_id.clone());
            task.batch_root_id.clone_from(&root);
            task.round = current.round;
            task.chain_depth = current.chain_depth + 1;
            task.body = Some("继续修改并复核".into());
            s.enqueue_task(&task).unwrap();
        }
        s.complete_delivery(&current.id, Status::Completed, Some(&format!("@{target} 继续修改并复核")), None, 1)
            .unwrap();
        let queued = s.queued_deliveries("g").unwrap();
        assert_eq!(queued.len(), 1, "每次只产生一份后续派工");
        current = queued.into_iter().next().unwrap();
    }
    assert!(current.round > 3 && current.chain_depth > 8);
    assert!(s.queued_deliveries("g").unwrap().is_empty());
    let (total, speaks): (i64, i64) =
        s.db.locked()
            .query_row(
                "SELECT COUNT(*),SUM(kind='speak') FROM deliveries WHERE batch_root_id=? AND status='completed'",
                [root],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(total, 25);
    assert!(speaks > 10);
    // 用户的新回复开始新请求，历史完成记录不会被改回 queued。
    s.post_group("g", &json!({"text":"用户确认，请继续下一项"}), &["b".into()]).unwrap();
    assert_eq!(s.queued_deliveries("g").unwrap().len(), 1);
    assert_eq!(s.delivery(&current.id).unwrap().unwrap().status, Status::Completed);
}

/// 升级不重放旧阈值取消的投递；新的用户请求拥有独立身份。
#[test]
fn historical_limit_cancellation_is_not_replayed_by_recovery_or_new_input() {
    let s = store();
    s.post_group("g", &json!({"text":"旧任务"}), &["a".into()]).unwrap();
    let old = s.queued_deliveries("g").unwrap().remove(0);
    s.cancel_deliveries("g", None, "对话轮次上限").unwrap();
    s.recover_deliveries().unwrap();
    assert!(s.claim_delivery(&old.id, "a").unwrap().0.is_none());
    s.post_group("g", &json!({"text":"用户明确继续旧任务"}), &["a".into()]).unwrap();
    let next = s.queued_deliveries("g").unwrap().remove(0);
    assert_ne!(next.id, old.id);
    assert_eq!(s.delivery(&old.id).unwrap().unwrap().status, Status::Cancelled);
    assert_eq!(s.delivery(&old.id).unwrap().unwrap().last_error.as_deref(), Some("对话轮次上限"));
}
