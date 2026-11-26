//! 通过 stdio 转发本机连接器，管理子进程、权限预检和工具回执。

use super::{GROUP_SESSION_ENV, connector_message, receipts};
use anyhow::Result;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 私密凭据只注入 connector；宿主绑定的代理按行检查新工具，旧 CLI 仍保留原 stdio 行为。
pub async fn serve_local(connector: String, bot: Option<String>, port: u16) -> Result<()> {
    use std::process::Stdio;
    let token = tokio::fs::read_to_string(crate::service::data_dir().join("token")).await?;
    let res = crate::http()
        .post(format!("http://127.0.0.1:{port}/api/connectorRuntime"))
        .bearer_auth(token.trim())
        .json(&json!({"id":connector,"botId":bot}))
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("Connector credentials unavailable; check Credentials in Codync");
    }
    let v: Value = res.json().await?;
    let command = v["command"].as_str().ok_or_else(|| anyhow::anyhow!("Not a local connector"))?;
    let mut child = tokio::process::Command::new(crate::shell::program(command));
    child.args(v["args"].as_array().into_iter().flatten().filter_map(Value::as_str));
    for (k, v) in v["env"].as_object().into_iter().flatten() {
        if let Some(v) = v.as_str() {
            child.env(k, v);
        }
    }
    let guarded = bot.is_some();
    let mut child = child
        .stdin(if guarded { Stdio::piped() } else { Stdio::inherit() })
        .stdout(if guarded { Stdio::piped() } else { Stdio::inherit() })
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let status = if let Some(bot) = bot {
        let mut input = child.stdin.take().ok_or_else(|| anyhow::anyhow!("connector stdin unavailable"))?;
        let output = child.stdout.take().ok_or_else(|| anyhow::anyhow!("connector stdout unavailable"))?;
        let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
        let writer = stdout.clone();
        let group = std::env::var(GROUP_SESSION_ENV).ok();
        let receipt_bridge = receipts::Bridge::new(port, token.trim(), &bot, group.as_deref());
        let pending = Arc::new(receipts::Pending::default());
        let responses = pending.clone();
        let (retire, mut retiring) = tokio::sync::watch::channel(false);
        // server 主动通知、反向请求与并发响应独立转发，不等待工具预检或改写 server ID。
        let mut reader = tokio::spawn(async move {
            let mut output = BufReader::new(output);
            let mut line = Vec::new();
            while output.read_until(b'\n', &mut line).await? != 0 {
                // 子进程退出时立即解除当前存档等待，尾部多条响应均以原始字节继续排空。
                if !*retiring.borrow()
                    && let Ok(mut message) = serde_json::from_slice::<Value>(&line)
                {
                    let shortened = tokio::select! {
                        biased;
                        _ = retiring.changed() => false,
                        saved = tokio::time::timeout(Duration::from_millis(650), responses.shorten(&mut message)) => saved == Ok(true),
                    };
                    if shortened {
                        line = serde_json::to_vec(&message)?;
                        line.push(b'\n');
                    }
                }
                let mut writer = writer.lock().await;
                writer.write_all(&line).await?;
                writer.flush().await?;
                line.clear();
            }
            anyhow::Ok(())
        });
        let mut incoming = Box::pin(async move {
            let mut incoming = BufReader::new(tokio::io::stdin());
            let mut line = Vec::new();
            while incoming.read_until(b'\n', &mut line).await? != 0 {
                if let Ok(message) = serde_json::from_slice::<Value>(&line) {
                    let original_count = message.as_array().map(Vec::len);
                    let (forward, refused, bound) =
                        connector_message(message, &connector, Some(&bot), port, token.trim(), Some(&receipt_bridge))
                            .await;
                    if !refused.is_empty() {
                        // 拒绝响应与 server 共用同一行级输出锁，不能把两个 JSON 响应交错写入 stdout。
                        let mut writer = stdout.lock().await;
                        for response in &refused {
                            let mut bytes = serde_json::to_vec(response)?;
                            bytes.push(b'\n');
                            writer.write_all(&bytes).await?;
                        }
                        writer.flush().await?;
                    }
                    if let Some(forward) = forward {
                        pending.forward(&forward, bound.as_ref());
                        if original_count == forward.as_array().map(Vec::len) {
                            input.write_all(&line).await?;
                        } else {
                            let mut bytes = serde_json::to_vec(&forward)?;
                            bytes.push(b'\n');
                            input.write_all(&bytes).await?;
                        }
                        input.flush().await?;
                    }
                } else {
                    // 旧 server 的非 JSON 输入仍透传；无效 JSON 不会被误造为一个新请求。
                    input.write_all(&line).await?;
                    input.flush().await?;
                }
                line.clear();
            }
            input.shutdown().await?;
            anyhow::Ok(())
        });
        let mut reader_done = false;
        // 双向流的错误都结束代理；正常输入 EOF 先关闭子进程 stdin，让它完成已有响应。
        let status: Result<_> = tokio::select! {
            status = child.wait() => status.map_err(anyhow::Error::from),
            result = &mut incoming => {
                drop(incoming);
                match result {
                    Ok(()) => child.wait().await.map_err(anyhow::Error::from),
                    Err(error) => Err(error),
                }
            },
            result = &mut reader => {
                reader_done = true;
                drop(incoming);
                match result {
                    Ok(Ok(())) => {
                        // MCP stdout 已关闭便不会再有响应；只给正在退出的进程短暂收尾时间。
                        match tokio::time::timeout(Duration::from_secs(1),child.wait()).await {
                            Ok(status) => status.map_err(anyhow::Error::from),
                            Err(_) => Err(anyhow::anyhow!("Connector closed stdout without exiting")),
                        }
                    },
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(error.into()),
                }
            }
        };
        if status.is_err() {
            let _ = child.kill().await;
        }
        if !reader_done {
            retire.send_replace(true);
            drain_connector_output(reader).await?;
        }
        status?
    } else {
        child.wait().await?
    };
    if !status.success() {
        anyhow::bail!("Connector exited unsuccessfully");
    }
    Ok(())
}

/// 主进程退出后短暂排空响应；后代持有 stdout 时终止并回收读取任务，不能挂住代理。
async fn drain_connector_output(mut reader: tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(1), &mut reader).await {
        result?
    } else {
        reader.abort();
        // abort 后必须等待释放管道和输出锁，不能留下脱离代理生命周期的读取任务。
        match reader.await {
            Err(error) if error.is_cancelled() => Ok(()),
            result => result?,
        }
    }
}

#[cfg(test)]
mod connector_tests {
    use super::*;

    /// 正常尾消息必须排空；模拟后代持管不关闭时，收尾仍有界并确实回收读取任务。
    #[tokio::test]
    async fn connector_output_drains_tail_and_reclaims_a_reader_with_an_inherited_open_pipe() {
        use tokio::sync::mpsc;
        for hold in [false, true] {
            let (mut writer, input) = tokio::io::duplex(256);
            let (sent, mut received) = mpsc::unbounded_channel();
            // 与生产一样逐行读取；保留写端模拟主进程已退、后代仍持有 stdout。
            let reader = tokio::spawn(async move {
                let mut input = BufReader::new(input);
                let mut line = Vec::new();
                while input.read_until(b'\n', &mut line).await? != 0 {
                    sent.send(std::mem::take(&mut line))?;
                }
                anyhow::Ok(())
            });
            let handle = reader.abort_handle();
            writer.write_all(b"{\"method\":\"notification\"}\n").await.unwrap();
            if !hold {
                writer.shutdown().await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(3), drain_connector_output(reader)).await.unwrap().unwrap();
            assert!(handle.is_finished());
            assert_eq!(received.recv().await.unwrap(), b"{\"method\":\"notification\"}\n");
            assert!(received.recv().await.is_none(), "reader must release its channel before returning");
        }
    }

    /// 旧 CLI 无绑定以及初始化、通知和反向响应不访问维护门禁，也不改写消息。
    #[tokio::test]
    async fn readonly_and_unbound_connector_messages_remain_unchanged() {
        for message in [
            json!({"id":1,"method":"initialize"}),
            json!({"method":"notifications/initialized"}),
            json!({"id":"server-request","result":{"ok":true}}),
        ] {
            assert_eq!(connector_message(message.clone(), "c", Some("bot"), 0, "token", None).await.0, Some(message));
        }
        let call = json!({"id":0,"method":"tools/call"});
        assert_eq!(connector_message(call.clone(), "c", None, 0, "token", None).await.0, Some(call));
    }

    /// 维护拒绝批量工具时保留数字/字符串 ID、只读消息及 server 响应，不回应通知。
    #[tokio::test]
    async fn denied_connector_batches_keep_readonly_messages_and_original_ids() {
        use axum::{Json, Router, http::StatusCode, routing::post};
        // 门禁必须收到宿主绑定的 bot，不能使用工具 arguments 中伪造的来源。
        let router = Router::new().route(
            "/api/connectorRuntime",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(
                    body,
                    json!({"id":"connector","botId":"trusted-bot","method":"tools/call","checkOnly":true})
                );
                (StatusCode::BAD_REQUEST, Json(json!({"error":"tools are unavailable during context maintenance"})))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // 测试结束主动终止自己的门禁服务，不遗留端口或后台进程。
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let initialize = json!({"id":1,"method":"initialize"});
        let notification = json!({"method":"notifications/cancelled","params":{"requestId":0}});
        let response = json!({"id":"from-server","result":{"ok":true}});
        let batch = json!([initialize,{"id":0,"method":"tools/call","params":{"arguments":{"botId":"other"}}},{"id":"work","method":"tools/call"},{"method":"tools/call"},notification,response]);
        let (forward, refused, bound) =
            connector_message(batch, "connector", Some("trusted-bot"), port, "token", None).await;
        assert!(bound.is_none());
        assert_eq!(forward, Some(json!([initialize, notification, response])));
        assert_eq!(refused.len(), 2);
        assert_eq!(refused[0]["id"], 0);
        assert_eq!(refused[1]["id"], "work");
        assert_eq!(refused[0]["error"]["code"], -32603);
        server.abort();
        let _ = server.await;
    }
}
