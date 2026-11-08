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
        // 所有选择方式复用原请求方授权，不能借检索或批量范围越权。
        for selection in [json!({}), json!({"query": "short"}), json!({"ranges": [{"offset": 0, "limit": 5}]})] {
            let mut args = json!({"requestId": request});
            args.as_object_mut().unwrap().extend(selection.as_object().unwrap().clone());
            assert!(call(&fixture.hub, caller, "read_bot_reply", &args).is_err());
        }
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
        json!({"query": ""}),
        json!({"query": null}),
        json!({"query": "a\nb"}),
        json!({"query": "甲".repeat(201)}),
        json!({"query": "short", "offset": 1}),
        json!({"ranges": []}),
        json!({"ranges": null}),
        json!({"ranges": vec![json!({"offset": 0, "limit": 1}); 9]}),
        json!({"ranges": [{"offset": 0, "limit": 2}], "limit": 2}),
        json!({"ranges": [{"offset": 0, "limit": 2}], "query": "short"}),
        json!({"ranges": [{"offset": 0}]}),
        json!({"ranges": [{"offset": 0, "limit": 1, "path": "/tmp/private"}]}),
        json!({"ranges": [{"offset": 0, "limit": 0}]}),
        json!({"ranges": [{"offset": -1, "limit": 1}]}),
        json!({"ranges": [{"offset": 0, "limit": 2500}, {"offset": 3000, "limit": 2500}]}),
        json!({"ranges": [{"offset": 0, "limit": 3}, {"offset": 2, "limit": 1}]}),
        json!({"ranges": [{"offset": u64::MAX, "limit": 1}]}),
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
    let result =
        call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "query": "original"})).unwrap();
    assert_eq!(result["complete"], false);
    assert_eq!(result["selectionComplete"], true);
    assert_eq!(result["matches"][0]["text"], "reply: original");
    fixture.shutdown().await;
}

/// 一次字面检索只返回匹配完整行，预算边界按 Unicode 计数并可继续取下一批。
#[tokio::test]
async fn query_returns_bounded_matching_lines_without_replaying_work() {
    let message = "序言🙂\nPASS 无关记录\nERROR 中间证据🙂\nPASS 其他\nERROR 第二条";
    let (fixture, request) = completed(message).await;
    let expected = format!("reply: {message}");
    let limit = "ERROR 中间证据🙂\n".chars().count();
    let result =
        call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "query": "ERROR", "limit": limit}))
            .unwrap();
    assert_eq!(result["text"], "");
    assert_eq!(result["source"], "trace");
    assert_eq!(result["complete"], true);
    assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    assert_eq!(result["matches"][0]["text"], "ERROR 中间证据🙂\n");
    assert_eq!(result["selectionComplete"], false);
    let offset = result["nextOffset"].as_u64().unwrap();
    assert_eq!(usize::try_from(offset).unwrap(), expected[..expected.find("ERROR 第二条").unwrap()].chars().count());
    let next =
        call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "query": "ERROR", "offset": offset}))
            .unwrap();
    assert_eq!(next["matches"][0]["text"], "ERROR 第二条");
    assert_eq!(next["selectionComplete"], true);
    assert!(next["nextOffset"].is_null());
    for query in ["error", "ERROR.*", "不存在"] {
        let empty = call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "query": query})).unwrap();
        assert_eq!(empty["matches"], json!([]));
        assert_eq!(empty["selectionComplete"], true);
    }
    fixture.shutdown().await;
}

/// 批量取回相隔很远的 Unicode 范围，不读取两段之间的无关正文。
#[tokio::test]
async fn ranges_return_only_requested_slices_in_one_read() {
    let message = format!("{}中间证据{}", "甲🙂".repeat(1000), "乙".repeat(1000));
    let (fixture, request) = completed(&message).await;
    let offset = "reply: ".chars().count();
    let result = call(
        &fixture.hub,
        "a",
        "read_bot_reply",
        &json!({"requestId": request, "ranges": [
            {"offset": offset, "limit": 2}, {"offset": offset + 2000, "limit": 4}
        ]}),
    )
    .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["text"], "");
    assert_eq!(result["selection"], "ranges");
    assert_eq!(
        result["slices"],
        json!([{ "offset": offset, "text": "甲🙂" }, { "offset": offset + 2000, "text": "中间证据" }])
    );
    fixture.shutdown().await;
}

/// 单条匹配行超预算时明确拒绝，不把缺失结尾的证据伪装成完整匹配。
#[tokio::test]
async fn query_rejects_a_line_larger_than_the_character_budget() {
    let (fixture, request) = completed(&format!("needle {}", "甲".repeat(4000))).await;
    assert!(
        call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "query": "needle", "limit": 4000}))
            .is_err()
    );
    let result =
        call(&fixture.hub, "a", "read_bot_reply", &json!({"requestId": request, "offset": 0, "limit": 20})).unwrap();
    assert!(result["text"].as_str().unwrap().contains("needle"));
    assert!(result["nextOffset"].is_number());
    fixture.shutdown().await;
}

/// 大量短行也受条数约束，连续取回不丢匹配，防止元数据超过正文预算数倍。
#[tokio::test]
async fn query_bounds_match_metadata_and_resumes_without_losing_lines() {
    let (fixture, request) = completed(&"hit\n".repeat(100)).await;
    let mut offset = 0;
    let mut count = 0;
    loop {
        let result = call(
            &fixture.hub,
            "a",
            "read_bot_reply",
            &json!({"requestId": request, "query": "hit", "offset": offset, "limit": 4000}),
        )
        .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert!(matches.len() <= MAX_MATCHES);
        assert!(serde_json::to_string(&result).unwrap().len() < 3000);
        count += matches.len();
        let Some(next) = result["nextOffset"].as_u64() else { break };
        assert!(next > offset);
        assert_eq!(result["selectionComplete"], false);
        offset = next;
    }
    assert_eq!(count, 100);
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
