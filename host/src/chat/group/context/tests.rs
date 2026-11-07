use super::*;
use crate::store::{EntryKind, Lane, Store, deliveries::Status};
use serde_json::json;
use std::sync::Arc;

struct Fixture {
    hub: Arc<Hub>,
    dir: std::path::PathBuf,
}
impl Fixture {
    /// 只建立隔离存储，不启动 agent；输入包和检索使用同一个真实数据库。
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("codync-context-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        for id in ["a", "b"] {
            store
                .save_bot(&serde_json::from_value(json!({"id":id,"name":id,"backend":"fixture","cwd":dir})).unwrap())
                .unwrap();
        }
        for id in ["g", "other"] {
            store
                .save_bot(
                    &serde_json::from_value(json!({"id":id,"name":id,"kind":"group","members":["a","b"]})).unwrap(),
                )
                .unwrap();
        }
        let hub = Hub::new(
            store,
            "context-test".into(),
            crate::remote::identity::Identity::load_or_create(&dir).unwrap(),
            "test".into(),
            0,
        );
        Self { hub, dir }
    }
    /// 原始用户消息和其一次已保存回复，派生第二名成员的真实投递。
    fn relay(&self, text: &str) -> (Entry, Delivery) {
        let root = self.hub.post_group("g", &json!({"text":text}), &["a".into()]).unwrap();
        let first = self.hub.store.queued_deliveries("g").unwrap().remove(0);
        self.hub.store.claim_delivery(&first.id, "a").unwrap();
        self.hub.store.start_delivery(&first.id, "a").unwrap();
        self.hub.store.complete_delivery(&first.id, Status::Completed, Some("@b 请复核全部要求"), None, 1).unwrap();
        let next = self.hub.store.queued_deliveries("g").unwrap().remove(0);
        (root, next)
    }
    /// 获取当前受指派成员的配置，避免用私聊配置替代群资格。
    fn member(&self) -> BotConfig {
        self.hub.store.bot("b").unwrap().unwrap().config
    }
    /// 建立已提交的真实群执行，检索测试仍经过工具入口和每页权限校验。
    fn activate(&self, delivery: &Delivery) {
        self.hub.store.claim_delivery(&delivery.id, &delivery.target_bot_id).unwrap();
        self.hub.store.start_delivery(&delivery.id, &delivery.target_bot_id).unwrap();
        self.hub.team.start_turn(
            &delivery.target_bot_id,
            crate::chat::team::Turn::Delivery { room: delivery.room_id.clone(), id: delivery.id.clone() },
        );
    }
    /// 为检索参数补可信当前投递，调用生产工具分发而非绕过鉴权的内部分页器。
    async fn read(&self, delivery: &Delivery, mut args: serde_json::Value) -> Result<serde_json::Value> {
        args["contextDeliveryId"] = json!(delivery.id);
        crate::chat::memory::tool_call(&self.hub, &delivery.target_bot_id, "search_history", &args).await
    }
}
impl Drop for Fixture {
    /// 仅清理此夹具的身份目录。
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 大量背景、多次回复和独立新请求不会挤掉原任务、负面要求与开放计划。
#[test]
fn protected_task_and_user_changes_survive_a_bounded_background() {
    let f = Fixture::new();
    let original = format!("{}\nROOT_TAIL 不要替换用户配置", "原始规范𠀀😀e\u{301}".repeat(1_000));
    let (root, next) = f.relay(&original);
    let update =
        f.hub.post_group("g", &json!({"text":"新增要求：保留 KEY_NO_REPLACE 与所有待办。"}), &["a".into()]).unwrap();
    assert_ne!(update.id, next.batch_root_id.as_deref().unwrap());
    f.hub.add_entry(&Lane::main("g"),EntryKind::Plan,1,&json!({"author":"a","deliveryId":next.id,"entries":[{"content":"TODO_EXACT 路径与截止时间不能丢","status":"pending"},{"content":"CLOSED_PLAN_SHOULD_NOT_REAPPEAR","status":"completed"}]})).unwrap();
    for i in 0..80 {
        f.hub
            .add_entry(
                &Lane::main("g"),
                EntryKind::Agent,
                1,
                &json!({"text":format!("background {i}: {}","重复讨论😀𠀀".repeat(1_000)),"author":"a","final":true}),
            )
            .unwrap();
    }
    let packet = delivery_prompt(&f.hub, &next, &f.member()).unwrap();
    assert!(packet.prompt.contains(&original));
    assert!(packet.prompt.contains("ROOT_TAIL 不要替换用户配置") && packet.prompt.contains("KEY_NO_REPLACE"));
    assert!(packet.prompt.contains("TODO_EXACT 路径与截止时间不能丢"));
    assert!(!packet.prompt.contains("CLOSED_PLAN_SHOULD_NOT_REAPPEAR"));
    assert!(packet.prompt.contains("independent requests"));
    assert!(packet.protected_estimate > BACKGROUND_TOKENS);
    assert!(packet.background_estimate <= BACKGROUND_TOKENS);
    assert_eq!(packet.shown.iter().filter(|id| *id == &root.id).count(), 1);
    assert!(packet.shown.contains(&update.id));
    assert!(packet.shown.len() < 20, "背景预算不等于携带全部群消息");
}

/// 后到消息不能因为前一条回复的 seq 比它大而被当成已读跳过。
#[test]
fn messages_arriving_during_a_turn_remain_available_to_the_next_turn() {
    let f = Fixture::new();
    let (root, next) = f.relay("原任务");
    let update =
        f.hub.post_group("g", &json!({"text":"DURING_EXECUTION 必须保留新增验收条件"}), &["a".into()]).unwrap();
    f.hub
        .add_entry(
            &Lane::main("g"),
            EntryKind::Agent,
            1,
            &json!({"text":"later saved reply","author":"b","final":true}),
        )
        .unwrap();
    let packet = delivery_prompt(&f.hub, &next, &f.member()).unwrap();
    assert!(packet.prompt.contains("DURING_EXECUTION"));
    assert!(packet.shown.contains(&update.id));
    assert!(packet.shown.contains(&root.id));
}

/// 原文按稳定 ID 无损分页；大型附件/引用元数据另行分页，也不能悄悄截断。
#[tokio::test]
async fn group_history_pages_original_unicode_and_large_metadata_without_loss() {
    let f = Fixture::new();
    let body = "原文𠀀😀e\u{301} with code `x`\n".repeat(1_400);
    let (root, next) = f.relay(&body);
    f.hub.store.claim_delivery(&next.id, "b").unwrap();
    f.hub.store.start_delivery(&next.id, "b").unwrap();
    f.hub.team.start_turn("b", crate::chat::team::Turn::Delivery { room: "g".into(), id: next.id.clone() });
    let mut offset = 0;
    let mut restored = String::new();
    let mut version = serde_json::Value::Null;
    loop {
        let page = crate::chat::memory::tool_call(
            &f.hub,
            "b",
            "search_history",
            &json!({"contextDeliveryId":next.id,"entryId":root.id,"offset":offset,"version":version,"limit":4096}),
        )
        .await
        .unwrap();
        let wire = page.to_string();
        assert!(wire.len() <= 8_192 && estimate(&wire) <= 2_048);
        restored.push_str(page["text"].as_str().unwrap());
        version = page["version"].clone();
        if let Some(n) = page["nextOffset"].as_u64() {
            assert!(n > offset);
            offset = n;
        } else {
            assert_eq!(page["complete"], true);
            break;
        }
    }
    assert_eq!(restored, body);
    let files = (0..160)
        .map(|i| json!({"id":format!("file{i}"),"name":format!("{}-{i}.txt","长路径𠀀".repeat(40))}))
        .collect::<Vec<_>>();
    let e =
        f.hub.add_entry(&Lane::main("g"), EntryKind::User, 1, &json!({"text":"files","attachments":files})).unwrap();
    let page = crate::chat::memory::tool_call(
        &f.hub,
        "b",
        "search_history",
        &json!({"contextDeliveryId":next.id,"entryId":e.id}),
    )
    .await
    .unwrap();
    assert_eq!(page["metadata"]["readWithPart"], "metadata");
    let mut metadata = String::new();
    let mut offset = 0;
    let mut version = serde_json::Value::Null;
    loop {
        let page = crate::chat::memory::tool_call(
            &f.hub,
            "b",
            "search_history",
            &json!({"contextDeliveryId":next.id,"entryId":e.id,"part":"metadata","offset":offset,"version":version,"limit":4096}),
        )
        .await
        .unwrap();
        assert!(page.to_string().len() <= 8_192 && estimate(&page.to_string()) <= 2_048);
        metadata.push_str(page["text"].as_str().unwrap());
        version = page["version"].clone();
        match page["nextOffset"].as_u64() {
            Some(n) => offset = n,
            None => break,
        }
    }
    assert_eq!(serde_json::from_str::<serde_json::Value>(&metadata).unwrap()["attachments"], json!(files));
}

/// 群检索拒绝私聊、其他群、过期执行和被移除成员，不泄漏思考与工具 trace。
#[tokio::test]
async fn group_history_scope_is_checked_on_every_page_and_never_becomes_private_search() {
    let f = Fixture::new();
    let (_, next) = f.relay("任务");
    f.hub.store.claim_delivery(&next.id, "b").unwrap();
    f.hub.store.start_delivery(&next.id, "b").unwrap();
    f.hub.team.start_turn("b", crate::chat::team::Turn::Delivery { room: "g".into(), id: next.id.clone() });
    let private = f.hub.add_entry(&Lane::main("b"), EntryKind::User, 1, &json!({"text":"PRIVATE_SECRET"})).unwrap();
    let other = f.hub.add_entry(&Lane::main("other"), EntryKind::User, 1, &json!({"text":"OTHER_SECRET"})).unwrap();
    let thought = f.hub.add_entry(&Lane::main("g"), EntryKind::Thought, 1, &json!({"text":"THOUGHT_SECRET"})).unwrap();
    for id in [private.id, other.id, thought.id] {
        let bad = crate::chat::memory::tool_call(
            &f.hub,
            "b",
            "search_history",
            &json!({"contextDeliveryId":next.id,"entryId":id}),
        )
        .await;
        assert!(bad.is_err());
    }
    assert!(
        crate::chat::memory::tool_call(
            &f.hub,
            "b",
            "search_history",
            &json!({"contextDeliveryId":"forged","query":""})
        )
        .await
        .is_err()
    );
    let mut group = f.hub.store.bot("g").unwrap().unwrap().config;
    group.members = vec!["a".into()];
    f.hub.store.save_bot(&group).unwrap();
    assert!(
        crate::chat::memory::tool_call(&f.hub, "b", "search_history", &json!({"contextDeliveryId":next.id}))
            .await
            .is_err()
    );
}

/// 同一成员不同投递及旧格式计划各保留开放项，后来的完成计划不能隐式取代它们。
#[test]
fn newer_plans_do_not_hide_another_delivery_or_legacy_open_work() {
    let f = Fixture::new();
    let (root, next) = f.relay("保留所有独立待办");
    let mut other = Delivery::speak("g", "unused", "b");
    other.kind = Kind::Task;
    other.source_kind = "tool".into();
    other.source_entry_id = None;
    other.source_bot_id = Some("a".into());
    other.batch_root_id = Some(root.id);
    other.body = Some("独立检查".into());
    let other = f.hub.store.enqueue_task(&other).unwrap().0;
    for (delivery, content, status) in [
        (Some(next.id.as_str()), "FIRST_OPEN_CHECK", "pending"),
        (None, "LEGACY_OPEN_CHECK", "in_progress"),
        (Some(other.id.as_str()), "SECOND_OPEN_CHECK", "pending"),
        (Some(other.id.as_str()), "ONLY_THIS_PLAN_IS_DONE", "completed"),
    ] {
        f.hub
            .add_entry(
                &Lane::main("g"),
                EntryKind::Plan,
                1,
                &json!({"author":"b","deliveryId":delivery,"entries":[{"content":content,"status":status}]}),
            )
            .unwrap();
    }
    let packet = delivery_prompt(&f.hub, &next, &f.member()).unwrap();
    for content in ["FIRST_OPEN_CHECK", "LEGACY_OPEN_CHECK", "SECOND_OPEN_CHECK"] {
        assert!(packet.prompt.contains(content), "missing {content}");
    }
    assert!(!packet.prompt.contains("ONLY_THIS_PLAN_IS_DONE"));
}

/// 计划变长、变短或同长改写都不能与上一版本的分页拼接；重新读取应得到新版本。
#[tokio::test]
async fn changed_plan_pages_require_a_fresh_read_even_when_the_length_is_unchanged() {
    let f = Fixture::new();
    let (_, next) = f.relay("检查计划分页");
    f.activate(&next);
    let plan = f
        .hub
        .add_entry(
            &Lane::main("g"),
            EntryKind::Plan,
            1,
            &json!({"author":"a","entries":[{"content":"a".repeat(1_200),"status":"pending"}]}),
        )
        .unwrap();
    for replacement in ["b".repeat(6_000), "b".repeat(300), "b".repeat(1_200)] {
        f.hub
            .set_entry(&plan.id, &json!({"author":"a","entries":[{"content":"a".repeat(1_200),"status":"pending"}]}))
            .unwrap();
        let first = f.read(&next, json!({"entryId":plan.id,"limit":32})).await.unwrap();
        let offset = first["nextOffset"].as_u64().unwrap();
        assert!(f.read(&next, json!({"entryId":plan.id,"offset":offset})).await.is_err());
        f.hub
            .set_entry(&plan.id, &json!({"author":"a","entries":[{"content":replacement,"status":"pending"}]}))
            .unwrap();
        let error =
            f.read(&next, json!({"entryId":plan.id,"offset":offset,"version":first["version"]})).await.unwrap_err();
        assert!(error.to_string().contains("restart at offset=0"));
        let fresh = f.read(&next, json!({"entryId":plan.id,"limit":32})).await.unwrap();
        assert_ne!(fresh["version"], first["version"]);
        assert_eq!(fresh["offset"], 0);
    }
    // 正文不变而引用元数据改写，也不能继续拼接上一版本的 metadata JSON。
    let entry = f
        .hub
        .add_entry(
            &Lane::main("g"),
            EntryKind::User,
            1,
            &json!({"text":"stable body","replyTo":{"text":"x".repeat(3_000)}}),
        )
        .unwrap();
    let first = f.read(&next, json!({"entryId":entry.id,"part":"metadata","limit":32})).await.unwrap();
    f.hub.set_entry(&entry.id, &json!({"text":"stable body","replyTo":{"text":"y".repeat(3_000)}})).unwrap();
    assert!(
        f.read(
            &next,
            json!({"entryId":entry.id,"part":"metadata","offset":first["nextOffset"],"version":first["version"]})
        )
        .await
        .is_err()
    );
}

/// 同名分工在改名后仍携带原始 UTF-16 身份映射，附件及被引用长原文不被截断。
#[test]
fn protected_mentions_keep_role_bindings_after_rename_with_files_and_full_quotes() {
    let f = Fixture::new();
    for id in ["a", "b"] {
        let mut bot = f.hub.store.bot(id).unwrap().unwrap().config;
        bot.name = "同名".into();
        f.hub.store.save_bot(&bot).unwrap();
    }
    let quote_text = format!("{}QUOTE_REQUIRED_TAIL", "引用𠀀😀e\u{301}".repeat(700));
    let quoted = f.hub.add_entry(&Lane::main("g"), EntryKind::Agent, 1,
        &json!({"author":"a","final":true,"text":quote_text,"attachments":[{"id":uuid::Uuid::new_v4().to_string(),"name":"quoted.png","size":7}]})).unwrap();
    let text = "😀 @同名 负责实现；@同名 负责复核";
    // 代理对前缀占两个 UTF-16 单元；同名两处分别绑定不同稳定 ID。
    let mentions = text
        .match_indices("@同名")
        .zip(["a", "b"])
        .map(|((at, token), id)| {
            let start = text[..at].encode_utf16().count();
            json!({"botId":id,"start":start,"end":start+token.encode_utf16().count()})
        })
        .collect::<Vec<_>>();
    let root = f
        .hub
        .post_group(
            "g",
            &json!({"text":text,"mentions":mentions,
        "replyTo":{"id":quoted.id,"author":"a","text":"引用快照"},
        "attachments":[{"id":uuid::Uuid::new_v4().to_string(),"name":"需求𠀀.txt","size":11}]}),
            &["b".into()],
        )
        .unwrap();
    let next = f.hub.store.queued_deliveries("g").unwrap().remove(0);
    for (id, name) in [("a", "改名实现者"), ("b", "改名复核者")] {
        let mut bot = f.hub.store.bot(id).unwrap().unwrap().config;
        bot.name = name.into();
        f.hub.store.save_bot(&bot).unwrap();
    }
    let packet = delivery_prompt(&f.hub, &next, &f.member()).unwrap();
    for required in [text, quote_text.as_str(), "需求𠀀.txt", "quoted.png", "改名实现者", "改名复核者", "UTF-16"]
    {
        assert!(packet.prompt.contains(required), "missing {required}");
    }
    assert!(packet.prompt.contains(&json!(mentions).to_string()));
    assert_eq!(mentions[0]["start"], 3);
    assert_eq!(packet.shown.iter().filter(|id| *id == &root.id).count(), 1);
    assert_eq!(packet.shown.iter().filter(|id| *id == &quoted.id).count(), 1);
}

/// 三类长列表都由首条未显示记录续页；待办游标执行完成后仍可继续，不能跳过后续项。
#[tokio::test]
async fn discovery_pages_all_rows_within_budget_even_when_a_delivery_cursor_completes() {
    let f = Fixture::new();
    let (root, next) = f.relay("遍历所有材料");
    f.activate(&next);
    let mut messages = vec![root.id, next.source_entry_id.clone().unwrap()];
    let mut plans = Vec::new();
    let mut deliveries = vec![next.id.clone()];
    for i in 0..41 {
        let message = f
            .hub
            .post_group("g", &json!({"text":format!("row {i} {}", "消息😀\\\"\n".repeat(180))}), &["a".into()])
            .unwrap();
        messages.push(message.id);
        deliveries.push(f.hub.store.queued_deliveries("g").unwrap().last().unwrap().id.clone());
        plans.push(f.hub.add_entry(&Lane::main("g"), EntryKind::Plan, 1,
            &json!({"author":"b","entries":[{"content":format!("plan {i} {}", "长计划😀\\\"\n".repeat(180)),"status":"pending"}]})).unwrap().id);
    }
    messages.reverse();
    plans.reverse();
    for (kind, mut expected) in [("messages", messages), ("plans", plans), ("deliveries", deliveries)] {
        let mut from = serde_json::Value::Null;
        let mut found = Vec::new();
        let mut unique = HashSet::new();
        let mut pages = 0;
        loop {
            let page = f.read(&next, json!({"kind":kind,"from":from,"limit":30})).await.unwrap();
            let wire = page.to_string();
            assert!(wire.len() <= 8_192 && estimate(&wire) <= 2_048);
            let rows = page["results"].as_array().unwrap();
            if pages == 0 {
                assert!(rows.len() < 30, "必须触发响应预算裁切而非仅条数分页");
            }
            for row in rows {
                let id = row["id"].as_str().unwrap().to_owned();
                assert!(unique.insert(id.clone()), "duplicate {kind} row {id}");
                found.push(id);
            }
            pages += 1;
            assert!(pages < 100, "cursor must progress");
            if page["next"].is_null() {
                break;
            }
            // 游标行消失于待办谓词后，仍用其持久 room_seq 定位后面的记录。
            if pages == 1 && kind == "deliveries" {
                let id = page["next"].as_str().unwrap();
                let delivery = f.hub.store.delivery(id).unwrap().unwrap();
                f.hub.store.claim_delivery(id, &delivery.target_bot_id).unwrap();
                f.hub.store.start_delivery(id, &delivery.target_bot_id).unwrap();
                f.hub.store.complete_delivery(id, Status::Completed, Some("已完成，不派生任务"), None, 1).unwrap();
                expected.retain(|candidate| candidate != id);
            }
            from = page["next"].clone();
        }
        assert!(pages > 1);
        assert_eq!(found, expected, "missing or reordered {kind} rows");
    }
}

/// 跨群的各类游标均拒绝；翻页期间来源移除或执行取消后，不得继续读取本群材料。
#[tokio::test]
async fn group_history_rechecks_foreign_cursors_source_membership_and_cancelled_execution() {
    let f = Fixture::new();
    let (root, next) = f.relay(&"本群分页原文😀".repeat(300));
    f.activate(&next);
    let private = f.hub.add_entry(&Lane::main("b"), EntryKind::User, 1, &json!({"text":"private"})).unwrap();
    let other = f.hub.post_group("other", &json!({"text":"other"}), &["a".into()]).unwrap();
    let other_plan = f
        .hub
        .add_entry(
            &Lane::main("other"),
            EntryKind::Plan,
            1,
            &json!({"author":"a","entries":[{"content":"other plan","status":"pending"}]}),
        )
        .unwrap();
    let other_delivery = f.hub.store.queued_deliveries("other").unwrap().remove(0);
    for (kind, id) in [
        ("messages", private.id),
        ("messages", other.id),
        ("plans", other_plan.id.clone()),
        ("deliveries", other_delivery.id.clone()),
    ] {
        assert!(f.read(&next, json!({"kind":kind,"from":id})).await.is_err());
    }
    assert!(f.read(&next, json!({"entryId":other_plan.id})).await.is_err());
    assert!(f.read(&next, json!({"deliveryId":other_delivery.id})).await.is_err());
    let first = f.read(&next, json!({"entryId":root.id,"limit":32})).await.unwrap();
    let args = json!({"entryId":root.id,"offset":first["nextOffset"],"version":first["version"]});
    let mut group = f.hub.store.bot("g").unwrap().unwrap().config;
    group.members = vec!["b".into()];
    f.hub.store.save_bot(&group).unwrap();
    assert!(f.read(&next, args.clone()).await.is_err(), "来源成员移除后不可续页");
    group.members = vec!["a".into(), "b".into()];
    f.hub.store.save_bot(&group).unwrap();
    f.hub.store.cancel_deliveries("g", Some("b"), "测试取消").unwrap();
    assert!(f.read(&next, args).await.is_err(), "已取消执行不可续页");
}
