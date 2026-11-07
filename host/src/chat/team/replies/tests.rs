use super::*;
use crate::chat::team::{call, tests::Fixture};
use crate::store::{Lane, Store};

/// 用既有假执行器完成一次真实 actor 委派，不调用付费模型。
async fn completed(message: &str) -> (Fixture, String) {
    let fixture = Fixture::new();
    let receipt = call(&fixture.hub, "a", "message_bot", &json!({"botId": "b", "message": message})).unwrap();
    let request = receipt["requestId"].as_str().unwrap().to_owned();
    // 等待持久化完成而非流式文本出现，保证之后验证的是稳定交付。
    fixture
        .until(|| {
            fixture.hub.store.bot_request("a", &request).unwrap().is_some_and(|e| e.data["status"] == "completed")
        })
        .await;
    (fixture, request)
}

/// 中文与 emoji 按字符分页，超过通知摘要的中间内容仍能完整取回。
#[tokio::test]
async fn reads_complete_reply_in_unicode_pages() {
    let message = format!("{}中间证据{}", "甲🙂".repeat(2_000), "乙".repeat(2_000));
    let (fixture, request) = completed(&message).await;
    let expected = format!("reply: {message}");
    let mut offset = 0;
    let mut all = String::new();
    loop {
        let result =
            call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "offset": offset, "limit": 997}))
                .unwrap();
        assert_eq!(result["source"], "trace");
        assert_eq!(result["complete"], true);
        assert_eq!(result["status"], "completed");
        all.push_str(result["text"].as_str().unwrap());
        let Some(next) = result["nextOffset"].as_u64() else { break };
        offset = next;
    }
    assert_eq!(all, expected);
    let end =
        call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "offset": expected.chars().count()}))
            .unwrap();
    assert_eq!(end["text"], "");
    assert!(end["nextOffset"].is_null());
    fixture.shutdown().await;
}

/// 请求方身份来自调用通道，参数不能扩大到其他 bot 或任意文件。
#[tokio::test]
async fn rejects_other_bots_and_malformed_arguments() {
    let (fixture, request) = completed("short").await;
    for caller in ["b", "c"] {
        assert!(call(&fixture.hub, caller, "read_bot_reply", &json!({"requestId": request})).is_err());
    }
    for extra in [
        json!({"offset": -1}),
        json!({"offset": 0.5}),
        json!({"limit": 0}),
        json!({"limit": 4001}),
        json!({"limit": null}),
        json!({"path": "/tmp/private"}),
        json!({"botId": "b"}),
        json!({"offset": 9999}),
    ] {
        let mut args = json!({"requestId": request});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(call(&fixture.hub, "a", "read_bot_reply", &args).is_err(), "{args}");
    }
    assert!(call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": "missing"})).is_err());
    fixture.shutdown().await;
}

/// 旧记录仅有摘要时明确不完整，不从相邻 trace 猜测完整答案。
#[tokio::test]
async fn historical_reply_is_only_an_incomplete_summary() {
    let (fixture, request) = completed("original").await;
    let mut notice = fixture.hub.store.bot_request("a", &request).unwrap().unwrap();
    notice.data["botMessage"].as_object_mut().unwrap().remove("replySource");
    fixture.hub.set_entry(&notice.id, &notice.data).unwrap();
    let result = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
    assert_eq!(result["source"], "summary");
    assert_eq!(result["complete"], false);
    assert_eq!(result["text"], "reply: original");
    fixture.shutdown().await;
}

/// 内容或引用发生变化就失效，不能把同回合其他 trace 当作交付。
#[tokio::test]
async fn changed_or_rebound_trace_is_unavailable() {
    let (fixture, request) = completed("original").await;
    let notice = fixture.hub.store.bot_request("a", &request).unwrap().unwrap();
    let id = notice.data["botMessage"]["replySource"]["entryId"].as_str().unwrap();
    let mut trace = fixture.hub.store.entry(id).unwrap();
    trace.data["text"] = "changed private text".into();
    fixture.hub.set_entry(id, &trace.data).unwrap();
    let result = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
    assert_eq!(result["source"], "unavailable");
    assert_eq!(result["text"], "");
    let neighbor = fixture
        .hub
        .add_entry(&Lane::main("c"), EntryKind::Agent, trace.turn, &json!({"text": "reply: original"}))
        .unwrap();
    let mut rebound = notice.clone();
    rebound.data["botMessage"]["replySource"] = json!(ReplySource::new(&neighbor));
    fixture.hub.set_entry(&notice.id, &rebound.data).unwrap();
    let result = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
    assert_eq!(result["source"], "unavailable");
    fixture.shutdown().await;
}

/// 接收者删除后不复活 trace；原请求方删除后整个读取入口拒绝。
#[tokio::test]
async fn deleted_bots_do_not_expose_reply_content() {
    let (fixture, request) = completed("original").await;
    fixture.hub.delete_bot("b").unwrap();
    let result = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
    assert_eq!(result["source"], "unavailable");
    fixture.hub.delete_bot("a").unwrap();
    assert!(call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).is_err());
    fixture.shutdown().await;
}

/// 新连接能读到同一完成关联，读取本身不派发或唤醒执行器。
#[tokio::test]
async fn reply_binding_survives_reopening_store() {
    let (fixture, request) = completed("original").await;
    let store = Store::open(&fixture.dir.join("test.db")).unwrap();
    store.expire_pending().unwrap();
    let notice = store.bot_request("a", &request).unwrap().unwrap();
    let source: ReplySource = serde_json::from_value(notice.data["botMessage"]["replySource"].clone()).unwrap();
    assert_eq!(store.entry(&source.entry_id).unwrap().data["text"], "reply: original");
    for _ in 0..3 {
        let result = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
        assert_eq!(result["text"], "reply: original");
    }
    drop(store);
    fixture.shutdown().await;
}

/// 排队请求不暴露流式私有文本，取消请求不提供可读取的成功交付。
#[tokio::test]
async fn pending_and_cancelled_requests_do_not_return_trace() {
    let fixture = Fixture::new();
    let receipt = call(&fixture.hub, "a", "message_bot", &json!({"botId": "b", "message": "BLOCK wait"})).unwrap();
    let request = receipt["requestId"].as_str().unwrap();
    let pending = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
    assert_eq!(pending["source"], "pending");
    assert_eq!(pending["text"], "");
    fixture.hub.send_cmd("b", crate::agent::bot::Cmd::Stop).unwrap();
    // 等待取消通知稳定，验证读取不会重新派发这次执行。
    fixture
        .until(|| fixture.hub.store.bot_request("a", request).unwrap().is_some_and(|e| e.data["status"] == "cancelled"))
        .await;
    let cancelled = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request})).unwrap();
    assert_eq!(cancelled["source"], "unavailable");
    fixture.shutdown().await;
}
