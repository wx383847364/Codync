use super::*;
use crate::store::deliveries::Status;
use futures::StreamExt;

/// 群内忙成员继续原任务，空闲成员先答；补做时以持久投递保留完整的 @all 原文。
#[tokio::test]
async fn a_new_group_message_reaches_idle_members_while_an_older_turn_is_busy() {
    let room = Room::new();
    let old = room.send("@a BLOCK earlier assignment", None).await;
    let old_id = delivery_in_state(&room, &old.id, "started").await;
    std::fs::write(room.dir.join("b/release"), "").unwrap();
    let original = "@all verify the complete original assignment 𠀀🙂";
    let source = room.send(original, None).await;
    let card_id = format!("delivery-card-{}-1", source.id);
    let delivery_ids: Vec<_> = room.hub.store.entry(&card_id).unwrap().data["deliveryRows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["deliveryId"].as_str().unwrap().to_owned())
        .collect();
    let main = Lane::main(&room.group);
    // 在释放 A 之前观察 B 的最终回复，确认进度来自新调度而非旧任务结束。
    tokio::time::timeout(WAIT, async {
        while room.replies(&main).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("b replies while a is still working in the same group");
    assert_eq!(room.replies(&main), [("b".to_owned(), "reply: group".to_owned())]);
    assert_eq!(room.hub.store.delivery(&old_id).unwrap().unwrap().status, Status::Started);
    assert_eq!(room.hub.store.delivery(&delivery_ids[0]).unwrap().unwrap().status, Status::Queued);

    std::fs::write(room.dir.join("a/release"), "").unwrap();
    room.settle().await;
    for id in delivery_ids {
        assert_eq!(room.hub.store.delivery(&id).unwrap().unwrap().status, Status::Completed);
    }
    assert_eq!(room.replies(&main).iter().map(|(bot, _)| bot.as_str()).collect::<Vec<_>>(), ["b", "a", "a"]);
    let prompts = std::fs::read_to_string(room.dir.join("a/prompts.jsonl")).unwrap();
    assert_eq!(prompts.lines().count(), 2);
    let prompt: String = serde_json::from_str(prompts.lines().last().unwrap()).unwrap();
    assert!(prompt.contains(&format!("Request (complete original):\nUser: {original}")));
    room.shutdown().await;
}

/// 不同消息重叠时，群的待审批状态不能被名单中更靠前的工作成员遮住。
#[tokio::test]
async fn group_status_prioritizes_a_member_waiting_for_approval() {
    let room = Room::new();
    let lane = Lane::main(&room.group);
    // 模拟 A 在同群处理旧消息时，B 在新消息的执行中进入待审批。
    room.hub.set_runtime("a", |r| {
        r.status = BotStatus::Working;
        r.lane = Some(lane.clone());
    });
    room.hub.set_runtime("b", |r| {
        r.status = BotStatus::NeedsInput;
        r.lane = Some(lane);
    });
    let group = room.hub.bot_value_for_test(&room.group);
    assert_eq!(group["status"], "needsInput");
    assert_eq!(group["activity"], "b");
    room.shutdown().await;
}

/// 首个流chunk之前失去写权限也保留完整结果；故障可重连查看且不领取下一项。
#[tokio::test]
async fn readonly_completion_retains_full_text_and_manual_save_resumes_the_actor() {
    let room = Room::new();
    let first = room.send("@a BLOCK first", None).await;
    let second = room.send("@a second", None).await;
    tokio::time::timeout(WAIT, async {
        loop {
            let e = room.hub.store.entry(&format!("delivery-card-{}-1", first.id)).unwrap();
            if e.data["deliveryRows"][0]["status"] == "started" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    room.hub.store.delivery_test_readonly(true);
    std::fs::write(room.dir.join("a/release"), "").unwrap();
    tokio::time::timeout(WAIT, async {
        while room.hub.runtime("a").storage_error.is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let second = room.hub.store.entry(&format!("delivery-card-{}-1", second.id)).unwrap();
    let queued = second.data["deliveryRows"][0]["deliveryId"].as_str().unwrap();
    assert_eq!(room.hub.store.delivery(queued).unwrap().unwrap().status, Status::Queued);
    // 以最新rev增量重连，运行时故障仍出现在快照里，不能依赖touch_bot写库。
    let stream = crate::api::events_stream(&room.hub, room.hub.store.current_rev(), None, &Caller::Local).unwrap();
    let mut stream = Box::pin(stream);
    let mut found = false;
    for _ in 0..10 {
        let Some(event) = tokio::time::timeout(Duration::from_secs(1), stream.next()).await.ok().flatten() else {
            break;
        };
        if event["type"] == "deliveryStorageError" && event["botId"] == "a" && event["storageError"].is_string() {
            found = true;
            break;
        }
    }
    assert!(found);
    tokio::time::sleep(Duration::from_secs(16)).await;
    assert_eq!(room.hub.store.delivery(queued).unwrap().unwrap().status, Status::Queued);
    room.hub.store.delivery_test_readonly(false);
    // 五次自动重试耗尽后即使存储恢复也不偷偷继续；必须由人工重试保存。
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(room.hub.runtime("a").storage_error.is_some());
    assert_eq!(room.hub.store.delivery(queued).unwrap().unwrap().status, Status::Queued);
    assert_eq!(room.hub.runtime("a").lane.as_ref().unwrap().chat, room.group);
    room.hub.send_cmd("a", Cmd::RetryDeliverySave).unwrap();
    room.settle().await;
    assert!(room.hub.runtime("a").storage_error.is_none());
    let results = room.replies(&Lane::main(&room.group));
    assert!(results.iter().any(|(_, text)| text == "reply: group"), "all chunks survive the write outage");
    room.shutdown().await;
}

/// 等待聚合卡提交指定状态，返回该来源事件中的投递身份。
async fn delivery_in_state(room: &Room, source: &str, status: &str) -> String {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(card) = room.hub.store.entry(&format!("delivery-card-{source}-1"))
                && card.data["deliveryRows"][0]["status"] == status
            {
                return card.data["deliveryRows"][0]["deliveryId"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("delivery reaches expected state")
}

/// 模拟旧取消通知迟到：其事务之后接受的新投递必须继续执行。
#[tokio::test]
async fn delayed_group_cancel_does_not_stop_a_newly_accepted_delivery() {
    let room = Room::new();
    let old = room.send("@a BLOCK old", None).await;
    let old_id = delivery_in_state(&room, &old.id, "started").await;
    // 先只提交旧请求的取消，故意不通过Hub立即通知actor，制造消息延迟。
    room.hub.store.cancel_deliveries(&room.group, None, "old stop").unwrap();
    std::fs::write(room.dir.join("a/release"), "").unwrap();
    room.settle().await;
    std::fs::remove_file(room.dir.join("a/release")).unwrap();
    let new = room.send("@a BLOCK new", None).await;
    let new_id = delivery_in_state(&room, &new.id, "started").await;
    room.hub.send_cmd("a", Cmd::CancelGroup { chat: room.group.clone(), delivery_ids: vec![old_id] }).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(room.hub.store.delivery(&new_id).unwrap().unwrap().status, Status::Started);
    assert_eq!(room.hub.runtime("a").status, BotStatus::Working);
    std::fs::write(room.dir.join("a/release"), "").unwrap();
    room.settle().await;
    assert_eq!(room.hub.store.delivery(&new_id).unwrap().unwrap().status, Status::Completed);
    room.shutdown().await;
}

/// 群语音保留 call 来源用于 trace 与最终回复，派工内容仍不作为用户记忆来源。
#[tokio::test]
async fn group_voice_delivery_keeps_call_on_stream_and_atomic_result() {
    let room = Room::new();
    crate::chat::calls::heard(&room.hub, &room.group, "group-voice", "phone").unwrap();
    crate::api::dispatch(
        &room.hub,
        &Caller::Local,
        "send",
        json!({"botId":room.group,"text":"@a voice work","call":"group-voice"}),
    )
    .await
    .unwrap();
    room.settle().await;
    let entries = room.hub.store.history(&room.group, i64::MAX, 100).unwrap();
    let reply = entries.iter().find(|entry| entry.kind == "agent" && entry.data["final"] == true).unwrap();
    assert_eq!(reply.data["call"], "group-voice");
    assert!(
        entries
            .iter()
            .any(|entry| entry.kind == "agent" && entry.data["final"] != true && entry.data["call"] == "group-voice")
    );
    room.shutdown().await;
}

/// 成员空闲可能先于 runner 退出，最终事件仍必须把来源群恢复为空闲。
#[tokio::test]
async fn runner_exit_publishes_the_final_idle_room_state() {
    let room = Room::new();
    let mut events = room.hub.events.subscribe();
    room.send("@a ordinary request", None).await;
    room.settle().await;
    let mut last = None;
    while let Ok(event) = events.try_recv() {
        if event["type"] == "bot" && event["bot"]["id"] == room.group {
            last = Some(event["bot"]["status"].clone());
        }
    }
    assert_eq!(last, Some(json!("idle")));
    room.shutdown().await;
}

/// 会话切换等待与取消不影响当前投递；强制切换保存失败时必须保留旧会话。
#[tokio::test]
async fn new_session_keeps_the_old_session_until_interruption_is_saved() {
    let room = Room::new();
    let source = room.send("@a BLOCK session switch", None).await;
    let id = delivery_in_state(&room, &source.id, "started").await;
    let old_session = room.hub.store.bot("a").unwrap().unwrap().session_id;
    assert!(old_session.is_some());
    room.hub.send_cmd("a", Cmd::SessionSwitch { mode: "after".into() }).unwrap();
    tokio::time::timeout(WAIT, async {
        while !room.hub.runtime("a").pending_new_session {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(room.hub.store.delivery(&id).unwrap().unwrap().status, Status::Started);
    assert_eq!(room.hub.store.bot("a").unwrap().unwrap().session_id, old_session);
    room.hub.send_cmd("a", Cmd::SessionSwitch { mode: "cancel".into() }).unwrap();
    tokio::time::timeout(WAIT, async {
        while room.hub.runtime("a").pending_new_session {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(room.hub.store.delivery(&id).unwrap().unwrap().status, Status::Started);
    // 强制取消可以结束 agent，但只读存储不能让旧会话提前消失。
    room.hub.store.delivery_test_readonly(true);
    room.hub.send_cmd("a", Cmd::SessionSwitch { mode: "immediate".into() }).unwrap();
    tokio::time::timeout(WAIT, async {
        while room.hub.runtime("a").storage_error.is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(room.hub.runtime("a").pending_new_session);
    assert_eq!(room.hub.store.bot("a").unwrap().unwrap().session_id, old_session);
    assert_eq!(room.hub.store.delivery(&id).unwrap().unwrap().status, Status::Started);
    room.hub.store.delivery_test_readonly(false);
    room.hub.send_cmd("a", Cmd::RetryDeliverySave).unwrap();
    room.settle().await;
    assert_eq!(room.hub.store.delivery(&id).unwrap().unwrap().status, Status::Interrupted);
    assert!(room.hub.store.bot("a").unwrap().unwrap().session_id.is_none());
    assert!(!room.hub.runtime("a").pending_new_session);
    room.shutdown().await;
}

/// 复制完成时重新检查持久状态和目标聊天，Stop 后不得把结果发布到群或私聊。
#[tokio::test]
async fn group_file_publication_checks_delivery_and_stop_before_writing() {
    let room = Room::new();
    let source = room.send("@a BLOCK file", None).await;
    let id = delivery_in_state(&room, &source.id, "started").await;
    let data = json!({"author":"a","text":"file","final":true,"files":[{"id":"test"}]});
    assert!(room.hub.publish_delivery_file(&id, "b", &Lane::main(&room.group), 1, &data).is_err());
    assert!(room.hub.publish_delivery_file(&id, "a", &Lane::main("a"), 1, &data).is_err());
    let published = room.hub.publish_delivery_file(&id, "a", &Lane::main(&room.group), 1, &data).unwrap();
    assert_eq!(published.bot_id, room.group);
    room.hub.cancel_deliveries(&room.group, None, "test stop").unwrap();
    assert!(room.hub.publish_delivery_file(&id, "a", &Lane::main(&room.group), 1, &data).is_err());
    room.shutdown().await;
}

/// 发起成员已退群而取消通知尚未到达时，受托成员也不能继续发布文件。
#[tokio::test]
async fn group_files_recheck_the_task_sender_before_publication() {
    let room = Room::new();
    let mut task = crate::store::deliveries::Delivery::speak(&room.group, "", "a");
    task.kind = crate::store::deliveries::Kind::Task;
    task.source_kind = "tool".into();
    task.source_bot_id = Some("b".into());
    task.body = Some("share a report".into());
    let task = room.hub.enqueue_delivery(&task).unwrap();
    room.hub.claim_delivery(&task.id, "a").unwrap().unwrap();
    assert!(room.hub.start_delivery(&task.id, "a").unwrap());
    let mut config = room.hub.store.bot(&room.group).unwrap().unwrap().config;
    config.members.retain(|id| id != "b");
    room.hub.store.save_bot(&config).unwrap();
    assert!(
        room.hub
            .publish_delivery_file(
                &task.id,
                "a",
                &Lane::main(&room.group),
                1,
                &json!({"author":"a","final":true,"files":[]})
            )
            .is_err()
    );
    room.shutdown().await;
}
