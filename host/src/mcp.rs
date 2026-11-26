//! Built-in stdio MCP servers: `chat` sends the user messages, reacts to messages, changes the bot's name and instructions and saves skills; `team` discovers and asks other bots; `manage`
//! creates bots and changes any bot's setup, and `computer` operates the desktop, when enabled. Calls go through authenticated loopback
//! HTTP to the running host, which owns delegation and screen control.

use anyhow::Result;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

mod local;
pub(crate) mod receipts;
mod remote;
pub use local::serve_local;
pub use remote::serve_remote;

/// 工具桥从 ACP 服务器配置读取群身份，不接收模型提供的群 ID。
pub(crate) const GROUP_SESSION_ENV: &str = "CODYNC_MCP_GROUP_SESSION";

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
/// 读取会话工具桥的群身份并在每个 HTTP 请求中带上。
pub async fn serve(bot: String, port: u16, server: Server) -> Result<()> {
    let token = std::fs::read_to_string(crate::service::data_dir().join("token")).unwrap_or_default();
    let token: Arc<str> = token.trim().into();
    let bot: Arc<str> = bot.into();
    let group_session = std::env::var(GROUP_SESSION_ENV).ok().map(Arc::<str>::from);
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
                list.extend(crate::local_tasks::tools());
                list.push(receipts::tool());
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
    let receipt_bridge = (!matches!(server, Server::Network))
        .then(|| receipts::Bridge::new(port, &token, &bot, group_session.as_deref()));
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
        let group_session = group_session.clone();
        let receipt_bridge = receipt_bridge.clone();
        // 单次调用在原有 RPC 响应前压缩，与其它请求独立。
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
                "tools/call" => {
                    if matches!(server, Server::Chat)
                        && params["name"] == receipts::READ
                        && let Some(bridge) = &receipt_bridge
                    {
                        Ok(bridge.read(&params["arguments"]).await)
                    } else {
                        let bound = match &receipt_bridge {
                            Some(bridge) => bridge.capture().await,
                            None => None,
                        };
                        let mut result = call(port, &token, &bot, group_session.as_deref(), params, server).await;
                        if let Some(bound) = bound {
                            bound.shorten(&mut result).await;
                        }
                        Ok(result)
                    }
                }
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
/// 使用工具桥绑定的群身份，宿主用它拒绝切换会话后的迟到请求。
async fn call(port: u16, token: &str, bot: &str, group_session: Option<&str>, params: &Value, server: Server) -> Value {
    let body = json!({
        "botId": bot,
        "groupSession": group_session,
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
        Server::Chat if params["name"] == crate::local_tasks::START => {
            ("chatCall", crate::chat::manage::APPROVAL_TIMEOUT + Duration::from_secs(10))
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
async fn connector_message(
    message: Value,
    connector: &str,
    bot: Option<&str>,
    port: u16,
    token: &str,
    bridge: Option<&receipts::Bridge>,
) -> (Option<Value>, Vec<Value>, Option<receipts::Bound>) {
    let Some(bot) = bot.filter(|_| connector_method(&message) == Some("tools/call")) else {
        return (Some(message), vec![], None);
    };
    // 不读取或重复解析 connector 凭据；host 的 checkOnly 在解锁 vault 前返回。
    let checked: Result<Option<receipts::Bound>> = async {
        let mut body = json!({"id":connector,"botId":bot,"method":"tools/call","checkOnly":true});
        if let Some(bridge) = bridge {
            body["receiptScope"] = true.into();
            body["groupSession"] = json!(bridge.group());
        }
        let response = crate::http()
            .post(format!("http://127.0.0.1:{port}/api/connectorRuntime"))
            .bearer_auth(token)
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        if !response.status().is_success() {
            let error: Value = response.json().await?;
            anyhow::bail!("{}", error["error"].as_str().unwrap_or("the Codync host refused the call"));
        }
        let Some(bridge) = bridge else { return Ok(None) };
        let scope = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|mut response| response.get_mut("receiptScope").map(Value::take));
        Ok(scope.and_then(|scope| bridge.bind(scope)))
    }
    .await;
    let error = match checked {
        Ok(bound) => return (Some(message), vec![], bound),
        Err(error) => error,
    };
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
    (forward, refused, None)
}
