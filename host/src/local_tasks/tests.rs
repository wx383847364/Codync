use super::*;
use crate::store::{BotConfig, StageGroup, Store, deliveries::LocalTaskResult};

/// 停止和旧执行的失败回执都不能向来源群发布证据；普通群和阶段群共用边界。
#[test]
fn local_task_only_finishes_the_current_live_delivery() {
    for stage in [None, Some(StageGroup::default())] {
        let store = Store::open(Path::new(":memory:")).expect("test store");
        let bot: BotConfig = serde_json::from_value(json!({"id":"qa","name":"QA","backend":"claude"})).expect("bot");
        let mut room: BotConfig =
            serde_json::from_value(json!({"id":"room","name":"Room","kind":"group","members":["qa"]})).expect("room");
        room.stage = stage;
        store.save_bot(&bot).expect("save bot");
        store.save_bot(&room).expect("save room");
        store.post_group("room", &json!({"text":"check"}), &["qa".into()]).expect("post");
        let queued = store.queued_deliveries("room").expect("queue").remove(0);
        store.claim_delivery(&queued.id, "qa").expect("claim");
        store.start_delivery(&queued.id, "qa").expect("start");
        let current = store.delivery(&queued.id).expect("read").expect("delivery");
        let mut result = LocalTaskResult {
            started_at: None,
            status: Status::Failed,
            detail: "Blocked",
            text: "Fixture blocked",
            files: &[],
            follow_up: false,
            turn: 1,
            guidance: &[],
        };
        assert!(store.complete_local_task(&current.id, &result).expect("stale result").is_empty());
        result.started_at = current.started_at;
        let entries = store.complete_local_task(&current.id, &result).expect("result");
        let report = entries.iter().find(|entry| entry.data["localTask"] == true).expect("visible result");
        assert!(entries.iter().any(|entry| entry.data["deliveryRows"][0]["resultId"] == report.id));
        assert!(store.complete_local_task(&current.id, &result).expect("duplicate result").is_empty());
        assert!(store.history("qa", i64::MAX, 10).expect("private chat").is_empty());
    }
}

/// 普通模型派工发过文件再失败时，原卡仍不会显示打不开的结果入口。
#[test]
fn failed_model_delivery_does_not_offer_a_file_as_its_result() {
    let store = Store::open(Path::new(":memory:")).expect("test store");
    for raw in [
        json!({"id":"qa","name":"QA","backend":"claude"}),
        json!({"id":"room","name":"Room","kind":"group","members":["qa"]}),
    ] {
        store.save_bot(&serde_json::from_value(raw).expect("bot")).expect("save");
    }
    store.post_group("room", &json!({"text":"check"}), &["qa".into()]).expect("post");
    let delivery = store.queued_deliveries("room").expect("queue").remove(0);
    store.claim_delivery(&delivery.id, "qa").expect("claim");
    store.start_delivery(&delivery.id, "qa").expect("start");
    store
        .insert_entry(
            &crate::store::Lane::main("room"),
            crate::store::EntryKind::Agent,
            1,
            &json!({"text":"Evidence","author":"qa","deliveryId":delivery.id,"final":true,
            "files":[{"id":"file","name":"check.txt","size":0,"sha256":""}]}),
        )
        .expect("file");
    let entries = store.complete_delivery(&delivery.id, Status::Failed, None, Some("error"), 1, &[]).expect("finish");
    let row = &entries.last().expect("card").data["deliveryRows"][0];
    assert_eq!(row["status"], "failed");
    assert!(row["resultId"].is_null());
}
