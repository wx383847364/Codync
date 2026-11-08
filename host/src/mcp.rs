//! Built-in stdio MCP servers: `chat` sends the user messages, reacts to messages, changes the bot's name and instructions and saves skills; `team` discovers and asks other bots; `manage`
//! creates bots and changes any bot's setup, and `computer` operates the desktop, when enabled. Calls go through authenticated loopback
//! HTTP to the running host, which owns delegation and screen control.

use anyhow::Result;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

mod remote;
pub use remote::serve_remote;

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// ACP `McpServer` for `codync-host mcp <args…> --port <port>`. Its env names the data directory, where the token
/// is: some agents start MCP servers with a filtered environment (Hermes keeps only PATH, HOME and a few more), so a
/// custom `CODYNC_HOME` wouldn't reach the server otherwise.
pub fn acp_server(name: &str, exe: &std::path::Path, args: &[&str], port: u16) -> Value {
    let port = port.to_string();
    let args: Vec<&str> = ["mcp"].into_iter().chain(args.iter().copied()).chain(["--port", &port]).collect();
    json!({
        "name": name, "command": exe, "args": args,
        "env": [{"name": "CODYNC_HOME", "value": crate::service::data_dir()}],
    })
}

#[derive(Clone, Copy)]
pub enum Server {
    Chat,
    Connectors,
    Routines,
    Computer,
    Team,
    Manage,
    Memory,
    Composio,
    /// For agents on this computer that aren't Codync bots; `bot` is the agent's name.
    Network,
}

const INSTRUCTIONS: &str = "Operate this computer's apps like a person would, in the background where you can: \
find the window (list_windows, list_apps, launch_app), look at it (get_window_state: its controls and a screenshot), \
act on a control by its element_token, then look again. Use pixel coordinates only for what has no control. \
Never type a password yourself: use type_login with a saved login (request_login asks the user for one). \
Don't approve payments: ask the user to do that from their phone.";

/// Serves MCP on stdio until the agent closes it.
pub async fn serve(bot: String, port: u16, server: Server) -> Result<()> {
    let token = std::fs::read_to_string(crate::service::data_dir().join("token")).unwrap_or_default();
    let token: Arc<str> = token.trim().into();
    let bot: Arc<str> = bot.into();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    // Requests run side by side (a manage approval can take minutes); their replies share stdout.
    let (replies, mut outbox) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(msg) = outbox.recv().await {
            let mut line = serde_json::to_vec(&msg)?;
            line.push(b'\n');
            out.write_all(&line).await?;
            out.flush().await?;
        }
        anyhow::Ok(())
    });
    let (name, instructions, available_tools) = match server {
        Server::Connectors => {
            ("codync-connectors", crate::market::requests::INSTRUCTIONS, crate::market::requests::tools())
        }
        // Listed by the host: they depend on this computer's driver.
        Server::Computer => ("codync-computer", INSTRUCTIONS, Value::Null),
        Server::Routines => ("codync-routines", crate::routines::INSTRUCTIONS, crate::routines::tools()),
        Server::Chat => {
            let mut tools = crate::chat::outbox::tools();
            if let Some(list) = tools.as_array_mut() {
                list.extend(crate::chat::artifacts::tools());
                list.extend(crate::chat::profile::tools());
                list.extend(crate::chat::fallbacks::tools());
                list.push(crate::chat::reactions::tool());
                list.push(crate::chat::skills::tool());
                list.extend(crate::dev_servers::tools());
            }
            ("codync-chat", crate::chat::outbox::INSTRUCTIONS, tools)
        }
        Server::Team => ("codync-team", crate::chat::team::INSTRUCTIONS, crate::chat::team::tools()),
        Server::Manage => ("codync-manage", crate::chat::manage::INSTRUCTIONS, crate::chat::manage::tools()),
        Server::Memory => ("codync-memory", crate::chat::memory::INSTRUCTIONS, crate::chat::memory::tools()),
        Server::Composio => {
            ("codync-composio", crate::market::composio::INSTRUCTIONS, crate::market::composio::tools())
        }
        // The one server agents that aren't bots add as `codync` (the cloud connector's name too); more tools join it.
        Server::Network => {
            ("codync", crate::chat::team::assistant::INSTRUCTIONS, crate::chat::team::assistant::tools())
        }
    };
    // Requests still running, by id: a cancelled one stops, and its host call with it (a team change's card expires).
    let mut running = std::collections::HashMap::<String, tokio::task::AbortHandle>::new();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").filter(|id| !id.is_null()).cloned() else {
            if msg["method"] == "notifications/cancelled"
                && let Some(request) = running.remove(&msg["params"]["requestId"].to_string())
            {
                request.abort();
            }
            continue; // other notifications (`notifications/initialized`…)
        };
        running.retain(|_, request| !request.is_finished());
        let key = id.to_string();
        let (replies, token, bot, available_tools) =
            (replies.clone(), token.clone(), bot.clone(), available_tools.clone());
        let request = tokio::spawn(async move {
            let params = &msg["params"];
            let reply = match msg["method"].as_str().unwrap_or_default() {
                "initialize" => Ok(json!({
                    "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": name, "version": env!("CARGO_PKG_VERSION")},
                    "instructions": instructions,
                })),
                "ping" => Ok(json!({})),
                "tools/list" => Ok(match server {
                    Server::Computer => computer_tools(port, &token).await,
                    _ => json!({"tools": available_tools}),
                }),
                "tools/call" => Ok(call(port, &token, &bot, params, server).await),
                method => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
            };
            let _ = replies.send(match reply {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
            });
        });
        running.insert(key, request.abort_handle());
    }
    // The agent is gone: replies still pending have no one to read them.
    Ok(())
}

/// The `computer` tools the host offers; none when it can't be reached.
async fn computer_tools(port: u16, token: &str) -> Value {
    let res = crate::http()
        .post(format!("http://127.0.0.1:{port}/api/computerTools"))
        .bearer_auth(token)
        .json(&json!({}))
        .timeout(Duration::from_secs(30))
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or_else(|_| json!({"tools": []})),
        _ => json!({"tools": []}),
    }
}

/// One tool call through the host. Failures are tool errors the agent can read, not protocol errors.
async fn call(port: u16, token: &str, bot: &str, params: &Value, server: Server) -> Value {
    let body = json!({
        "botId": bot,
        "name": params["name"],
        "arguments": params.get("arguments").filter(|a| a.is_object()).cloned().unwrap_or_else(|| json!({})),
    });
    let (method, timeout) = match server {
        Server::Connectors => ("connectorCall", Duration::from_secs(90)),
        Server::Routines => ("routineCall", Duration::from_secs(30)),
        Server::Computer => ("computerCall", Duration::from_mins(1)),
        // Starting a dev server may wait on an approval card, then for its port.
        Server::Chat if params["name"] == crate::dev_servers::START => {
            ("chatCall", crate::chat::manage::APPROVAL_TIMEOUT + Duration::from_secs(25))
        }
        Server::Chat => ("chatCall", Duration::from_secs(if params["name"] == "send_file" { 70 } else { 30 })),
        Server::Team => ("teamCall", Duration::from_secs(30)),
        Server::Manage => ("manageCall", crate::chat::manage::APPROVAL_TIMEOUT + Duration::from_secs(30)),
        Server::Memory => ("memoryCall", Duration::from_mins(2)),
        Server::Composio => ("composioCall", Duration::from_mins(2)),
        // The first listing may wait for the other computers.
        Server::Network => ("agentCall", Duration::from_secs(30)),
    };
    let res = crate::http()
        .post(format!("http://127.0.0.1:{port}/api/{method}"))
        .bearer_auth(token)
        .json(&body)
        .timeout(timeout)
        .send()
        .await;
    let error = match res {
        Ok(r) => {
            let ok = r.status().is_success();
            match r.json::<Value>().await {
                Ok(v) if ok => {
                    return match server {
                        Server::Computer if v["isError"] == true => json!({"content": v["content"], "isError": true}),
                        Server::Computer => json!({"content": v["content"]}),
                        Server::Chat
                        | Server::Team
                        | Server::Manage
                        | Server::Memory
                        | Server::Routines
                        | Server::Network
                        | Server::Connectors => {
                            json!({"content": [{"type": "text", "text": v.to_string()}]})
                        }
                        Server::Composio => json!({"content": [{"type": "text", "text": v["result"].to_string()}]}),
                    };
                }
                Ok(v) => v["error"].as_str().unwrap_or("the Codync host refused the call").to_owned(),
                Err(e) => format!("bad response from the Codync host: {e}"),
            }
        }
        Err(e) => format!("can't reach the Codync host: {e}"),
    };
    json!({"content": [{"type": "text", "text": error}], "isError": true})
}

/// 判断单条或旧版批量 MCP 消息是否包含工作工具，通知和只读初始化不受维护门禁限制。
pub(super) fn connector_method(message: &Value) -> Option<&str> {
    // 批量请求也必须检查，不能通过包装成数组绕过 tools/call 门禁。
    if message.as_array().is_some_and(|batch| batch.iter().any(|item| item["method"] == "tools/call")) {
        Some("tools/call")
    } else {
        message["method"].as_str()
    }
}

/// 转发前通过现有凭据入口只检查门禁；失败时仅拒绝工作工具，保留原请求 ID 与只读消息。
pub(super) async fn connector_message(
    message: Value,
    connector: &str,
    bot: Option<&str>,
    port: u16,
    token: &str,
) -> (Option<Value>, Vec<Value>) {
    let Some(bot) = bot.filter(|_| connector_method(&message) == Some("tools/call")) else {
        return (Some(message), vec![]);
    };
    // 不读取或重复解析 connector 凭据；host 的 checkOnly 在解锁 vault 前返回。
    let checked: Result<()> = async {
        let response = crate::http()
            .post(format!("http://127.0.0.1:{port}/api/connectorRuntime"))
            .bearer_auth(token)
            .json(&json!({"id":connector,"botId":bot,"method":"tools/call","checkOnly":true}))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        if !response.status().is_success() {
            let error: Value = response.json().await?;
            anyhow::bail!("{}", error["error"].as_str().unwrap_or("the Codync host refused the call"));
        }
        Ok(())
    }
    .await;
    let Err(error) = checked else { return (Some(message), vec![]) };
    let detail = error.to_string();
    let batched = message.is_array();
    let messages = match message {
        Value::Array(batch) => batch,
        single => vec![single],
    };
    let mut allowed = Vec::new();
    let mut refused = Vec::new();
    for message in messages {
        if message["method"] != "tools/call" {
            allowed.push(message);
        } else if let Some(id) = message.get("id").filter(|id| !id.is_null()) {
            // JSON-RPC 通知没有响应，数值和字符串 ID 均原样保留。
            refused.push(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":detail}}));
        }
    }
    let forward = if allowed.is_empty() {
        None
    } else if batched {
        Some(Value::Array(allowed))
    } else {
        allowed.pop()
    };
    (forward, refused)
}

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
        // server 主动通知、反向请求与并发响应独立转发，不等待工具预检或改写 server ID。
        let mut reader = tokio::spawn(async move {
            let mut output = BufReader::new(output);
            let mut line = Vec::new();
            while output.read_until(b'\n', &mut line).await? != 0 {
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
                    let (forward, refused) =
                        connector_message(message, &connector, Some(&bot), port, token.trim()).await;
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
            assert_eq!(connector_message(message.clone(), "c", Some("bot"), 0, "token").await, (Some(message), vec![]));
        }
        let call = json!({"id":0,"method":"tools/call"});
        assert_eq!(connector_message(call.clone(), "c", None, 0, "token").await, (Some(call), vec![]));
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
        let (forward, refused) = connector_message(batch, "connector", Some("trusted-bot"), port, "token").await;
        assert_eq!(forward, Some(json!([initialize, notification, response])));
        assert_eq!(refused.len(), 2);
        assert_eq!(refused[0]["id"], 0);
        assert_eq!(refused[1]["id"], "work");
        assert_eq!(refused[0]["error"]["code"], -32603);
        server.abort();
        let _ = server.await;
    }
}
