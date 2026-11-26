use super::*;

/// 摘要保留日志首尾及中间错误，文字再多也不把完整回执交给模型。
#[test]
fn summary_preserves_failure_head_and_tail_within_budget() {
    let body = format!("START\n{}\nERROR 截图纹理为空\n{}\nEND", "PASS\n".repeat(800), "info\n".repeat(800));
    assert!(eligible(&body));
    let summary = summarize("receipt", &body);
    for term in ["START", "ERROR 截图纹理为空", "END", READ, "receipt"] {
        assert!(summary.contains(term), "{term}");
    }
    assert!(summary.chars().count() < 2_600);
    assert!(!eligible(&"a".repeat(THRESHOLD)));
    assert!(!eligible(&"a".repeat(MAX_BYTES + 1)));
    assert!(!eligible(&format!("{body}\0")));
    assert!(!eligible(&json!({"rows":body}).to_string()));
}

/// 中文、表情和换行分页后能完全还原，隔离群和另一 Bot 无法读同一 ID。
#[test]
fn receipt_round_trips_unicode_and_requires_matching_owner() {
    let mut db = Connection::open_in_memory().unwrap();
    setup(&db).unwrap();
    let original = "录🧪\n".repeat(PAGE);
    let id = save(&mut db, "tester", "qa", &original, TTL, || Ok(())).unwrap();
    let mut offset = 0;
    let mut restored = String::new();
    loop {
        let page = read(&db, "tester", "qa", &id, offset, None, TTL).unwrap();
        restored.push_str(page["text"].as_str().unwrap());
        let Some(next) = page["nextOffset"].as_u64() else { break };
        offset = usize::try_from(next).unwrap();
    }
    assert_eq!(restored, original);
    let found = read(&db, "tester", "qa", &id, 4001, Some("录🧪"), TTL).unwrap();
    assert_eq!(found["matchOffset"], 4002);
    assert!(found["text"].as_str().unwrap().chars().count() <= 900);
    let missing = read(&db, "tester", "qa", &id, 0, Some("missing"), TTL).unwrap();
    assert!(missing["matchOffset"].is_null());
    assert!(missing["nextOffset"].is_null());
    for (bot, room, now) in
        [("other", "qa", TTL), ("tester", "other", TTL), ("tester", "", TTL), ("tester", "qa", 2 * TTL + 1)]
    {
        assert!(read(&db, bot, room, &id, 0, None, now).is_err());
    }
    assert!(read(&db, "tester", "qa", &id, original.chars().count() + 1, None, TTL).is_err());
    expire(&db, 2 * TTL + 1).unwrap();
    // 调整测试读取时间也无法再找到正文，验证过期清理确实删除了记录。
    assert!(read(&db, "tester", "qa", &id, 0, None, TTL).is_err());
}

/// 连接器乱序回复仍匹配正确请求。取消、反向请求和结构化响应均不进入存档。
#[tokio::test]
async fn connector_receipts_match_response_ids_and_preserve_rpc_content() {
    use axum::{Json, Router, routing::post};
    // 保存服务只接收预期的第二条回执，测试不读写用户缓存和网络外部服务。
    let app = Router::new().route(
        "/api/toolReceipt",
        post(|Json(body): Json<Value>| async move {
            assert_eq!(body["data"]["scope"]["generation"], 2);
            assert_eq!(body["op"], "save");
            Json(json!({"summary":"shortened"}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // 创建本测试独占的环回服务，结束后主动释放端口。
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let bridge = Bridge::new(port, "fixture", "qa", Some("group"));
    let pending = Pending::default();
    let original = "an original plain-text response\n".repeat(300);
    let response = json!({"content":[{"type":"text", "text":original}],"isError":true});
    let first = bridge.bind(json!({"room":"group","generation":1})).unwrap();
    let second = bridge.bind(json!({"room":"group","generation":2})).unwrap();

    for (id, bound) in [(json!("initial"), &first), (json!(2), &second), (json!(3), &first)] {
        pending.forward(&json!({"id":id,"method":"tools/call"}), Some(bound));
    }
    pending.forward(&json!({"method":"notifications/cancelled","params":{"requestId":"initial"}}), None);
    let mut reversed = json!({"id":2,"method":"ping"});
    assert!(!pending.shorten(&mut reversed).await);
    let mut second = json!({"id":2,"result":response});
    assert!(pending.shorten(&mut second).await);
    assert_eq!(second["result"]["content"][0]["text"], "shortened");
    assert_eq!(second["result"]["isError"], true);
    let mut cancelled = json!({"id":"initial","result":response});
    assert!(!pending.shorten(&mut cancelled).await);
    assert_eq!(cancelled["result"], response);
    let mut structured = json!({"id":3,"result":{"content":response["content"],"structuredContent":{"ok":true}}});
    let untouched = structured.clone();
    assert!(!pending.shorten(&mut structured).await);
    assert_eq!(structured, untouched);
    server.abort();
}
