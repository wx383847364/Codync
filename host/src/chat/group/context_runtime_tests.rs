//! 通过真实 ACP 子进程和 HTTP 工具桥验证领取前整理；完整 stdio MCP 由独立 E2E 覆盖。

use super::*;
use crate::store::deliveries::Status;
use serde_json::Value;
use tokio::sync::oneshot;

/// 复用 Room 的真实 actor，以独立端口开放生产 HTTP 工具桥，不运行单测二进制的 MCP CLI。
async fn context_room() -> (Room, tokio::task::JoinHandle<()>) {
    let room = Room::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = crate::api::router(room.hub.clone());
    // 每例独享监听端口；其余 Room 测试及正在运行的宿主不受影响。
    let server = tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    for bot in ["a", "b"] {
        script(&room, bot, "direct-api.json", &json!({"botId":bot,"port":port,"token":room.hub.token}));
    }
    (room, server)
}

/// 只关闭本用例的 HTTP 服务和 actor，并由既有 Room 释放临时原文与日志。
async fn close(room: Room, server: tokio::task::JoinHandle<()>) {
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    room.shutdown().await;
}

/// 复用群夹具的工作目录写脚本，所有临时记录随 Room 清理。
fn script(room: &Room, bot: &str, name: &str, value: &Value) {
    std::fs::write(room.dir.join(bot).join(name), value.to_string()).unwrap();
}

/// 日志尚未创建表示该动作未发生；其他读取错误直接暴露。
fn records(room: &Room, bot: &str, name: &str) -> Vec<Value> {
    let path = room.dir.join(bot).join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("{}: {error}", path.display()),
    };
    // ACP 子进程按行提交；调用方在阶段标记或 settle 后读取完整记录。
    text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// 等待夹具明确进入阶段，避免把 Working 或短暂 idle 当作维护已经开始/结束。
async fn marker(room: &Room, bot: &str, name: &str) {
    tokio::time::timeout(WAIT, async {
        while !room.dir.join(bot).join(name).exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture did not reach {bot}/{name}"));
}

/// 维护期间通过生产 HTTP 工具桥派工、发消息与写记忆，响应必须全部为拒绝。
fn forbidden_tools() -> Value {
    json!([
        {"server":"team","name":"message_bot","arguments":{"botId":"b","message":"COMPACT_PRIVATE_TASK"}},
        {"server":"chat","name":"send_message","arguments":{"text":"COMPACT_PRIVATE_MESSAGE"}},
        {"server":"memory","name":"edit_memory","arguments":{"file":"MEMORY.md","append":"COMPACT_PRIVATE_MEMORY"}}
    ])
}

/// 仅允许整理摘要进入私有检查点，群消息、trace、计划与成员私聊都不得出现维护正文。
fn assert_maintenance_is_private(room: &Room) {
    for lane in [room.group.as_str(), "a", "b"] {
        let entries = room.hub.store.history(lane, i64::MAX, 500).unwrap();
        assert!(
            !serde_json::to_string(&entries).unwrap().contains("COMPACT_PRIVATE"),
            "maintenance leaked into {lane}"
        );
    }
    let tools = records(room, "a", "compact-tools.jsonl");
    assert_eq!(tools.len(), 3);
    for tool in tools {
        assert_eq!(tool["response"]["isError"], true, "maintenance tool succeeded: {tool}");
    }
    let permission: Value =
        serde_json::from_str(&std::fs::read_to_string(room.dir.join("a/compact-permission.json")).unwrap()).unwrap();
    assert_eq!(permission["outcome"]["outcome"], "cancelled");
}

/// 长于旧轮数限制的文字/工具桥混合接力经历同 sid 维护后，原文、开放计划与完整结果都还在。
#[tokio::test]
async fn long_relay_compacts_the_same_session_without_losing_source_or_open_work() {
    let (room, server) = context_room().await;
    let original = format!("@a begin {} ROOT_TAIL_EXACT", "保留用户原始约束".repeat(600));
    let mut a = Vec::new();
    let mut b = Vec::new();
    for step in 0..13 {
        let peer = if step % 2 == 0 { "b" } else { "a" };
        let mut turn = if step == 12 {
            json!({"reply":"DELIVERED_ALL_THIRTEEN"})
        } else if step % 3 == 0 {
            json!({"tasks":[{"botId":peer,"message":format!("EXACT_TOOL_BODY_{step}")}],"reply":format!("assigned action {step}")})
        } else {
            json!({"reply":format!("@{peer} continue action {}",step+1)})
        };
        if step == 0 {
            turn["usage"] = json!({"used":70_000,"size":200_000});
            turn["plan"] = json!([{"content":"OPEN_PLAN_EXACT","status":"in_progress","priority":"high"}]);
        }
        if step % 2 == 0 { a.push(turn) } else { b.push(turn) }
    }
    script(&room, "a", "group-script.json", &json!({"steps":a}));
    script(&room, "b", "group-script.json", &json!({"steps":b}));
    script(
        &room,
        "a",
        "group-compact.json",
        &json!({
            "usage":{"used":20_000,"size":200_000},"tools":forbidden_tools(),"permission":true
        }),
    );
    room.send(&original, None).await;
    room.settle().await;

    let replies = room.replies(&Lane::main(&room.group));
    assert_eq!(replies.len(), 13, "maintenance is not another reply or relay step: {replies:?}");
    assert_eq!(replies.last().unwrap().1, "DELIVERED_ALL_THIRTEEN");
    assert_eq!(records(&room, "a", "compactions.jsonl").len(), 1);
    for (bot, expected) in [("a", 7), ("b", 6)] {
        let requests = records(&room, bot, "prompt-requests.jsonl");
        // 独立维护分支不能推进普通脚本，所有业务 prompt 都携带完整根要求。
        let normal: Vec<_> =
            requests.iter().filter(|r| !r["prompt"][0]["text"].as_str().unwrap().starts_with("/compact")).collect();
        assert_eq!(normal.len(), expected);
        for request in &normal {
            assert!(request["prompt"][0]["text"].as_str().unwrap().contains(&original));
        }
        assert!(normal.last().unwrap()["prompt"][0]["text"].as_str().unwrap().contains("OPEN_PLAN_EXACT"));
        assert!(requests.iter().all(|r| r["sessionId"] == requests[0]["sessionId"]));
        let sessions = records(&room, bot, "sessions.jsonl");
        assert_eq!(sessions.iter().filter(|r| r["method"] == "session/new").count(), 1, "no rollover");
    }
    for step in [0, 3, 6, 9] {
        let target = if step % 2 == 0 { "b" } else { "a" };
        // 工具指派正文不在展示性回复里，必须作为下一成员的完整来源重新注入。
        assert!(
            records(&room, target, "prompts.jsonl")
                .iter()
                .any(|p| p.as_str().unwrap().contains(&format!("EXACT_TOOL_BODY_{step}")))
        );
    }
    let sid = room.hub.store.bot("a").unwrap().unwrap().session_id.unwrap();
    assert_eq!(
        room.hub.store.kv_get(&crate::chat::handoff::summary_key("a", &sid)).as_deref(),
        Some("COMPACT_PRIVATE_SUMMARY")
    );
    let plans = room.hub.store.history(&room.group, i64::MAX, 500).unwrap();
    assert!(plans.iter().any(|e| e.kind == "plan" && e.data["entries"][0]["content"] == "OPEN_PLAN_EXACT"));
    let mut completed = std::collections::HashSet::new();
    for entry in &plans {
        if let Some(rows) = entry.data["deliveryRows"].as_array() {
            for row in rows {
                assert_eq!(row["status"], "completed", "accepted work did not finish: {row}");
                completed.insert(row["deliveryId"].as_str().unwrap());
            }
        }
    }
    assert_eq!(completed.len(), 13);
    assert_maintenance_is_private(&room);
    close(room, server).await;
}

/// 已确认但无收益和完全未确认的整理都只尝试一次；仍有足够空间的后续指派照常执行。
#[tokio::test]
async fn no_benefit_or_unconfirmed_compaction_does_not_loop_or_drop_accepted_work() {
    for confirmed in [true, false] {
        let (room, server) = context_room().await;
        script(
            &room,
            "a",
            "group-script.json",
            &json!({"steps":[
                {"reply":"seeded usage","usage":{"used":70_000,"size":200_000}},
                {"reply":"second accepted result"},{"reply":"third accepted result"}
            ]}),
        );
        script(
            &room,
            "a",
            "group-compact.json",
            &json!({"confirmed":confirmed,"usage":{"used":70_000,"size":200_000}}),
        );
        for text in ["@a seed", "@a second", "@a third"] {
            room.send(text, None).await;
            room.settle().await;
        }
        assert_eq!(records(&room, "a", "compactions.jsonl").len(), 1, "no-benefit confirmed={confirmed}");
        assert_eq!(std::fs::read_to_string(room.dir.join("a/group-script-step")).unwrap(), "3");
        assert_eq!(room.replies(&Lane::main(&room.group)).last().unwrap().1, "third accepted result");
        assert_eq!(room.hub.store.delivery_rooms(None).unwrap(), Vec::<String>::new());
        close(room, server).await;
    }
}

/// Stop 和强制 /new 的取消宽限期仍隔离迟到工具；只有群 Stop 取消来源 queued。
#[tokio::test]
async fn stopping_maintenance_preserves_its_scope_and_rejects_late_tools() {
    for action in ["member", "group", "new"] {
        let stop_group = action == "group";
        let (room, server) = context_room().await;
        script(
            &room,
            "a",
            "group-script.json",
            &json!({"steps":[
                {"reply":"seeded usage","usage":{"used":70_000,"size":200_000}},
                {"reply":"accepted task after maintenance"}
            ]}),
        );
        script(
            &room,
            "a",
            "group-compact.json",
            &json!({
                "hold":true,"holdAfterCancel":true,"confirmed":false,"tools":forbidden_tools(),"permission":true
            }),
        );
        room.send("@a seed", None).await;
        room.settle().await;
        let source = room.send("@a pending original", None).await;
        marker(&room, "a", "compactions.jsonl").await;
        let queued = room.hub.store.queued_deliveries(&room.group).unwrap();
        assert_eq!(queued.len(), 1);
        let id = queued[0].id.clone();
        assert_eq!(queued[0].source_entry_id.as_deref(), Some(source.id.as_str()));
        assert!(queued[0].claimed_at.is_none() && queued[0].started_at.is_none());
        if action == "new" {
            room.hub.send_cmd("a", Cmd::SessionSwitch { mode: "immediate".into() }).unwrap();
        } else {
            crate::api::dispatch(
                &room.hub,
                &Caller::Local,
                "stop",
                json!({"botId":if stop_group { room.group.as_str() } else { "a" }}),
            )
            .await
            .unwrap();
        }
        marker(&room, "a", "compact-cancelled").await;
        assert!(room.hub.team.maintaining("a"), "Stop must not lift isolation before ACP acknowledges cancellation");
        let expected = if stop_group { Status::Cancelled } else { Status::Queued };
        assert_eq!(room.hub.store.delivery(&id).unwrap().unwrap().status, expected);
        // 也覆盖已越过工具桥但尚未由 actor 处理的迟到消息和审批命令。
        let (reply, answer) = oneshot::channel();
        room.hub
            .send_cmd("a", Cmd::SendToUser { group_session: None, text: "COMPACT_PRIVATE_LATE_ACTOR".into(), reply })
            .unwrap();
        assert!(tokio::time::timeout(WAIT, answer).await.unwrap().unwrap().is_err());
        let (reply, answer) = oneshot::channel();
        room.hub
            .send_cmd(
                "a",
                Cmd::Approve {
                    group_session: None,
                    title: "COMPACT_PRIVATE_APPROVAL".into(),
                    detail: "late".into(),
                    reply,
                },
            )
            .unwrap();
        assert!(!tokio::time::timeout(WAIT, answer).await.unwrap().unwrap());
        std::fs::write(room.dir.join("a/compact-cancel-release"), "").unwrap();
        room.settle().await;
        assert_eq!(
            room.hub.store.delivery(&id).unwrap().unwrap().status,
            if stop_group { Status::Cancelled } else { Status::Completed }
        );
        assert_eq!(room.replies(&Lane::main(&room.group)).len(), if stop_group { 1 } else { 2 });
        assert_eq!(records(&room, "a", "compactions.jsonl").len(), 1);
        if action == "new" {
            // 新会话只在维护退出后建立，原业务行保持同一个身份并且只执行一次。
            assert!(!room.hub.runtime("a").pending_new_session);
            assert_eq!(
                records(&room, "a", "sessions.jsonl").iter().filter(|r| r["method"] == "session/new").count(),
                2
            );
            assert_eq!(std::fs::read_to_string(room.dir.join("a/group-script-step")).unwrap(), "2");
        }
        assert_maintenance_is_private(&room);
        close(room, server).await;
    }
}

/// 维护重载失败而新建会话时，结束维护身份；原 queued 只执行一次且业务工具恢复可用。
#[tokio::test]
async fn fresh_session_skips_maintenance_and_runs_the_queued_assignment_once() {
    let (room, server) = context_room().await;
    script(
        &room,
        "a",
        "group-script.json",
        &json!({"steps":[
            {"reply":"seeded usage","usage":{"used":70_000,"size":200_000}},
            {"tasks":[{"botId":"b","message":"fresh-session task exactly once"}],"reply":"completed after fresh session"},
            {"reply":"later assignment is a normal turn"}
        ]}),
    );
    script(&room, "b", "group-script.json", &json!({"reply":"fresh-session peer result"}));
    script(&room, "a", "group-compact.json", &json!({"usage":{"used":20_000,"size":200_000}}));
    room.send("@a seed", None).await;
    room.settle().await;
    std::fs::write(room.dir.join("a/reject-load"), "").unwrap();
    std::fs::write(room.dir.join("a/hold-session"), "").unwrap();
    // RefreshTools 保留旧 sid 和 usage，直到隐藏维护开始 ensure_session 时才重启连接。
    room.hub.send_cmd("a", Cmd::RefreshTools).unwrap();
    let source = room.send("@a accepted before reload failed", None).await;
    // 阻塞真实 session/load，先证明进入的是未领取业务的维护，而非普通执行重载失败。
    tokio::time::timeout(WAIT, async {
        while !room.hub.team.maintaining("a") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("context pressure starts maintenance before claiming the delivery");
    assert!(room.hub.store.queued_deliveries(&room.group).unwrap().iter().any(|d| {
        d.source_entry_id.as_deref() == Some(source.id.as_str()) && d.claimed_at.is_none() && d.started_at.is_none()
    }));
    std::fs::remove_file(room.dir.join("a/hold-session")).unwrap();
    room.settle().await;
    let card = room.hub.store.entry(&format!("delivery-card-{}-1", source.id)).unwrap();
    let id = card.data["deliveryRows"][0]["deliveryId"].as_str().unwrap();
    assert_eq!(room.hub.store.delivery(id).unwrap().unwrap().status, Status::Completed);
    assert!(!room.hub.team.maintaining("a"));
    assert!(records(&room, "a", "compactions.jsonl").is_empty(), "fresh session has nothing to compact");
    let sessions = records(&room, "a", "sessions.jsonl");
    // 真实 ACP 重载确实失败并走到 session/new，不能用从未进入维护的用例替代回归。
    assert!(sessions.iter().any(|r| r["method"] == "session/load"));
    assert_eq!(sessions.iter().filter(|r| r["method"] == "session/new").count(), 2);
    room.send("@a subsequent task", None).await;
    room.settle().await;
    assert_eq!(std::fs::read_to_string(room.dir.join("a/group-script-step")).unwrap(), "3");
    assert_eq!(records(&room, "b", "prompt-requests.jsonl").len(), 1);
    let replies = room.replies(&Lane::main(&room.group));
    assert_eq!(replies.len(), 4);
    assert_eq!(replies.last().unwrap().1, "later assignment is a normal turn");
    assert_eq!(room.hub.store.delivery_rooms(None).unwrap(), Vec::<String>::new());
    close(room, server).await;
}

/// 无收益抑制只针对不变上下文；业务真实增加 usage 后，下一次指派可再次整理并继续。
#[tokio::test]
async fn usage_growth_after_no_benefit_allows_another_compaction() {
    let (room, server) = context_room().await;
    script(
        &room,
        "a",
        "group-script.json",
        &json!({"steps":[
            {"reply":"seeded usage","usage":{"used":70_000,"size":200_000}},
            {"reply":"normal work grew the context","usage":{"used":80_000,"size":200_000}},
            {"reply":"continued after useful maintenance"}
        ]}),
    );
    script(&room, "a", "group-compact.json", &json!({"usage":{"used":70_000,"size":200_000}}));
    for text in ["@a seed", "@a complete the next action"] {
        room.send(text, None).await;
        room.settle().await;
    }
    assert_eq!(records(&room, "a", "compactions.jsonl").len(), 1);
    script(&room, "a", "group-compact.json", &json!({"usage":{"used":20_000,"size":200_000}}));
    room.send("@a continue after real progress", None).await;
    room.settle().await;
    assert_eq!(records(&room, "a", "compactions.jsonl").len(), 2, "new usage must invalidate no-benefit suppression");
    let replies = room.replies(&Lane::main(&room.group));
    assert_eq!(replies.len(), 3);
    assert_eq!(replies.last().unwrap().1, "continued after useful maintenance");
    assert_eq!(std::fs::read_to_string(room.dir.join("a/group-script-step")).unwrap(), "3");
    assert_eq!(room.hub.store.delivery_rooms(None).unwrap(), Vec::<String>::new());
    close(room, server).await;
}

/// 领取前预算涵盖 `FirstMessage` 补发及 put-back；超窗仍 queued，反复预检不消费提醒或发送 prompt。
#[tokio::test]
async fn first_message_profile_and_putback_are_budgeted_without_consuming_the_queued_task() {
    for large in ["profile", "putback"] {
        let (room, server) = context_room().await;
        std::fs::write(room.dir.join("a/first-message"), "").unwrap();
        if large == "profile" {
            room.hub
                .update_bot(
                    &json!({"id":"a","description":format!("PROFILE_EXACT {}", "保留完整成员说明".repeat(3_000))}),
                )
                .unwrap();
        }
        script(
            &room,
            "a",
            "group-script.json",
            &json!({"steps":[
                {"reply":"seeded first-message session","compacted":true,"usage":{"used":20_000,"size":30_000}},
                {"reply":"must not receive the oversized prompt"}
            ]}),
        );
        room.send("@a seed", None).await;
        room.settle().await;
        let row = room.hub.store.bot("a").unwrap().unwrap();
        let sid = row.session_id.unwrap();
        let why = if large == "putback" {
            format!("PUTBACK_EXACT {}", "原提醒不得预检消费".repeat(3_000))
        } else {
            "PUTBACK_EXACT keep this reminder until a real prompt is sent".to_owned()
        };
        crate::chat::memory::put_back(&room.hub.store, "a", &sid, "MEMORY.md", &why);
        let reminder = crate::chat::memory::peek_change_notice(&room.hub.store, &row.config, &sid, None).unwrap();
        let original = "@a preserve ORIGINAL_EXACT and finish the accepted task";
        let source = room.send(original, None).await;
        // 等待真实预检写出资源不足说明，不能以尚未调度的短暂 queued 作为通过证据。
        let blocked = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(delivery) = room.hub.store.queued_deliveries(&room.group).unwrap().into_iter().find(|d| {
                    d.source_entry_id.as_deref() == Some(source.id.as_str())
                        && d.last_error.as_deref().is_some_and(|error| error.contains("上下文空间不足"))
                }) {
                    break delivery;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("oversized FirstMessage additions must stay queued");
        let packet = delivery_prompt(&room.hub, &blocked, &row.config).unwrap();
        assert!(codync_mem::tokens::estimate(&packet.prompt) + 20_000 + 2_048 < 30_000, "task packet alone fits");
        assert!(blocked.claimed_at.is_none() && blocked.started_at.is_none());
        assert_eq!(room.hub.store.entry(&source.id).unwrap().data["text"], original);
        assert_eq!(
            crate::chat::memory::peek_change_notice(&room.hub.store, &row.config, &sid, None).unwrap(),
            reminder
        );
        assert_eq!(records(&room, "a", "prompt-requests.jsonl").len(), 1);
        assert_eq!(std::fs::read_to_string(room.dir.join("a/group-script-step")).unwrap(), "1");
        // 至少跨过一次周期补查，仍不得领取、不消耗待发提醒。
        tokio::time::sleep(Duration::from_millis(3_500)).await;
        assert_eq!(room.hub.store.delivery(&blocked.id).unwrap().unwrap().status, Status::Queued);
        assert_eq!(
            crate::chat::memory::peek_change_notice(&room.hub.store, &row.config, &sid, None).unwrap(),
            reminder
        );
        assert_eq!(records(&room, "a", "prompt-requests.jsonl").len(), 1);
        crate::api::dispatch(&room.hub, &Caller::Local, "stop", json!({"botId":room.group})).await.unwrap();
        room.settle().await;
        close(room, server).await;
    }
}

/// 编号群重新打开自己的模型会话，整理和业务提示始终使用相同的群会话身份。
#[tokio::test]
async fn group_session_reopens_and_compacts_without_using_the_shared_session() {
    let (room, server) = context_room().await;
    script(
        &room,
        "a",
        "group-script.json",
        &json!({"steps":[
            {"reply":"original reply"},
            {"reply":"first new group reply","usage":{"used":70_000,"size":200_000}},
            {"reply":"second new group reply"},
            {"reply":"third new group reply"}
        ]}),
    );
    script(&room, "a", "group-compact.json", &json!({"usage":{"used":20_000,"size":200_000}}));
    room.send("@a ORIGINAL_SESSION_MARKER", None).await;
    room.settle().await;
    let shared = room.hub.store.bot("a").unwrap().unwrap().session_id.unwrap();
    let created = room.hub.create_group_session(&room.group, None).unwrap();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["members"], json!(["a", "b"]));
    assert!(room.hub.store.history(id, i64::MAX, 500).unwrap().is_empty());

    for text in ["@a first new group message", "@a second new group message", "@a after reopening"] {
        if text == "@a after reopening" {
            // 模拟工具定义更新引起的进程重启，下一次请求须重开已保存的独立会话。
            room.hub.send_cmd("a", Cmd::RefreshTools).unwrap();
        }
        crate::api::dispatch(&room.hub, &Caller::Local, "send", json!({"botId":id,"text":text})).await.unwrap();
        room.settle().await;
    }

    let requests = records(&room, "a", "prompt-requests.jsonl");
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0]["sessionId"], shared);
    assert_ne!(requests[1]["sessionId"], shared);
    for request in &requests[1..] {
        assert_eq!(request["sessionId"], requests[1]["sessionId"]);
        assert!(!request["prompt"][0]["text"].as_str().unwrap().contains("ORIGINAL_SESSION_MARKER"));
    }
    assert!(requests[2]["prompt"][0]["text"].as_str().unwrap().starts_with("/compact"));
    assert!(records(&room, "a", "sessions.jsonl").iter().any(|request| {
        request["method"] == "session/load" && request["params"]["sessionId"] == requests[1]["sessionId"]
    }));
    assert_eq!(room.hub.store.bot("a").unwrap().unwrap().session_id.as_deref(), Some(shared.as_str()));
    close(room, server).await;
}
