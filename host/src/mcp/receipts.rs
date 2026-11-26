//! Host 在 MCP 回执到达模型前保留较长的纯文本，并提供按字符分页的原文读取。

use crate::hub::Hub;
use anyhow::{Result, anyhow, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(super) const READ: &str = "read_tool_receipt";
const THRESHOLD: usize = 6_000;
const MAX_BYTES: usize = 1_048_576;
const PAGE: usize = 4_000;
const TTL: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_RECEIPTS: i64 = 128;

/// 在工具列表登记原文读取能力，模型只需提供摘要里的回执 ID 和分页位置。
pub(super) fn tool() -> Value {
    json!({
        "name": READ,
        "description": "Read a page of the original text of a shortened Host tool result. Use the receiptId from the excerpt. For a known marker, set query to the exact text to retrieve a small excerpt around it. Otherwise page through the original with offset. Only call when the excerpts lack a detail you need.",
        "inputSchema": {
            "type":"object",
            "required":["receiptId"],
            "additionalProperties":false,
            "properties":{
                "receiptId":{"type":"string"},
                "offset":{"type":"integer","minimum":0},
                "query":{"type":"string","minLength":1,"maxLength":120,"description":"Find the first exact, case-sensitive occurrence at or after offset."}
            }
        }
    })
}

/// 获取可信归属。写入时还会核对投递，避免取消后的响应误入另一群。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Scope {
    room: Option<String>,
    generation: u64,
}

/// 将代理进程持有的身份绑定到宿主，不采信模型提供的 bot 或群标识。
#[derive(Clone)]
pub(super) struct Bridge {
    port: u16,
    token: Arc<str>,
    bot: Arc<str>,
    group: Option<Arc<str>>,
}

impl Bridge {
    /// 从宿主注入的桥环境读取隔离群归属。
    pub(super) fn new(port: u16, token: &str, bot: &str, group: Option<&str>) -> Self {
        Self { port, token: token.into(), bot: bot.into(), group: group.map(Into::into) }
    }

    /// 在连接器门禁请求中提供可信的隔离群绑定。
    pub(super) fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// 将门禁返回的当时回合与桥绑定，旧 Host 不返回归属时正常透传。
    pub(super) fn bind(&self, scope: Value) -> Option<Bound> {
        let scope = serde_json::from_value(scope).ok()?;
        Some(Bound { bridge: self.clone(), scope })
    }

    /// 使用本机专有接口存取回执，原文不随群消息同步。
    async fn request(&self, op: &str, data: Value) -> Result<Value> {
        let response = crate::http()
            .post(format!("http://127.0.0.1:{}/api/toolReceipt", self.port))
            .bearer_auth(&self.token)
            .json(&json!({"botId": self.bot.as_ref(), "groupSession": self.group.as_deref(), "op": op, "data": data}))
            .timeout(Duration::from_millis(650))
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(status.is_success(), "{}", value["error"].as_str().unwrap_or("tool receipt is unavailable"));
        Ok(value)
    }

    /// 在转发前记住当前投递，后续存盘失败时继续提供原回执。
    pub(super) async fn capture(&self) -> Option<Bound> {
        self.bind(self.request("scope", Value::Null).await.ok()?)
    }

    /// 分页回执直接交给模型，无需再次压缩。
    pub(super) async fn read(&self, args: &Value) -> Value {
        match self.request("read", args.clone()).await {
            Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}]}),
            Err(error) => json!({"isError":true,"content":[{"type":"text","text":error.to_string()}]}),
        }
    }
}

#[derive(Clone)]
pub(super) struct Bound {
    bridge: Bridge,
    scope: Scope,
}

impl Bound {
    /// 仅替换单段纯文本。结构化字段、图片、资源、注解及错误标志留给原工具协议处理。
    pub(super) async fn shorten(self, result: &mut Value) -> bool {
        if result.get("structuredContent").is_some() {
            return false;
        }
        let Some([block]) = result.get_mut("content").and_then(Value::as_array_mut).map(Vec::as_mut_slice) else {
            return false;
        };
        if block["type"] != "text" || block.as_object().is_none_or(|item| item.len() != 2) {
            return false;
        }
        let Some(text) = block["text"].as_str().filter(|text| eligible(text)) else { return false };
        // Base64 的展开比例有界，在现有本机 API 请求大小限制内保留任意 UTF-8 日志。
        let saved = self.bridge.request("save", json!({"scope":self.scope, "body":STANDARD.encode(text)})).await;
        if let Ok(saved) = saved
            && let Some(summary) = saved["summary"].as_str()
        {
            block["text"] = summary.into();
            return true;
        }
        false
    }
}

/// 连接器的工具调用和响应可能并发、乱序或走独立 SSE 流，以 JSON-RPC ID 关联。
#[derive(Default)]
pub(super) struct Pending(Mutex<HashMap<String, Bound>>);

impl Pending {
    /// 只记录已通过门禁的 tools/call，取消通知会移除对应回执。
    pub(super) fn forward(&self, message: &Value, bound: Option<&Bound>) {
        let messages = messages(message);
        let mut pending = crate::LockExt::locked(&self.0);
        for item in messages {
            if item["method"] == "notifications/cancelled" {
                pending.remove(&item["params"]["requestId"].to_string());
            } else if item["method"] == "tools/call"
                && let Some(id) = item.get("id").filter(|id| !id.is_null())
            {
                let id = id.to_string();
                pending.remove(&id);
                // 异常连接器可能永不返回。只限制可压缩的挂起数，工具调用照常转发。
                if pending.len() < 256
                    && let Some(bound) = bound
                {
                    pending.insert(id, bound.clone());
                }
            }
        }
    }

    /// 初始化响应、通知与反向请求原样转发；只处理已登记工具的最终响应。
    pub(super) async fn shorten(&self, message: &mut Value) -> bool {
        let items = if message.is_array() {
            message.as_array_mut().map(Vec::as_mut_slice).unwrap_or_default()
        } else {
            std::slice::from_mut(message)
        };
        let mut changed = false;
        for item in items {
            if item.get("method").is_some() || item.get("id").is_none() {
                continue;
            }
            let bound = crate::LockExt::locked(&self.0).remove(&item["id"].to_string());
            if let Some(bound) = bound
                && let Some(result) = item.get_mut("result")
            {
                changed |= bound.shorten(result).await;
            }
        }
        changed
    }
}

/// 将旧版批量消息与普通请求统一遍历。
fn messages(message: &Value) -> &[Value] {
    message.as_array().map_or_else(|| std::slice::from_ref(message), Vec::as_slice)
}

/// 保留机器可读 JSON、超出存档上限及含 NUL 的原始回执。
fn eligible(text: &str) -> bool {
    text.len() <= MAX_BYTES
        && text.chars().take(THRESHOLD + 1).count() > THRESHOLD
        && !text.contains('\0')
        && serde_json::from_str::<serde::de::IgnoredAny>(text).is_err()
}

/// 识别常用日志错误级别和异常关键词，按单词匹配以避免把普通标识纳入摘要。
fn failure_line(line: &str) -> bool {
    const MARKERS: &[&str] = &[
        "error",
        "failed",
        "failure",
        "fatal",
        "panic",
        "traceback",
        "exception",
        "assertionerror",
        "warn",
        "warning",
        "fail",
    ];
    ["错误", "失败", "异常"].iter().any(|marker| line.contains(marker))
        || line
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|word| MARKERS.iter().any(|marker| word.eq_ignore_ascii_case(marker)))
}

/// 先保留开头、结尾和典型失败行，失败行按首次出现顺序采样，所有片段都受字符上限约束。
fn summarize(id: &str, text: &str) -> String {
    let length = text.chars().count();
    let head: String = text.chars().take(420).collect();
    let tail: String = text.chars().skip(length - 620).collect();
    let mut failures = String::new();
    for (index, line) in text.lines().enumerate().filter(|(_, line)| failure_line(line)).take(8) {
        use std::fmt::Write;
        let _ = writeln!(failures, "L{} {}", index + 1, line.chars().take(160).collect::<String>());
    }
    let failure_excerpt = if failures.is_empty() { "No matching failure lines in the text.".into() } else { failures };
    format!(
        "[Host shortened this tool result. Original: {length} characters. If needed, use {READ} with receiptId \"{id}\" and a query for a known marker, or offset to page from character 0.]\n\nStart excerpt:\n{head}\n\nFailure / warning excerpts (selected):\n{failure_excerpt}\n\nEnd excerpt:\n{tail}"
    )
}

/// 用独立本机缓存存放原文，不进入聊天数据库、消息同步或群附件。
fn open(path: &Path) -> Result<Connection> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // 原文可能包含连接器返回的私密数据，缓存目录仅当前系统用户可遍历。
        let mut directory = std::fs::DirBuilder::new();
        directory.mode(0o700).recursive(true).create(path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    let db = Connection::open(path.join("receipts.sqlite3"))?;
    setup(&db)?;
    Ok(db)
}

/// 初始化缓存表与写入参数，生产磁盘库和内存回归测试共用同一合同。
fn setup(db: &Connection) -> Result<()> {
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch(
        "PRAGMA secure_delete=ON;
         CREATE TABLE IF NOT EXISTS receipts(
            id TEXT PRIMARY KEY, bot TEXT NOT NULL, room TEXT NOT NULL, created INTEGER NOT NULL, text TEXT NOT NULL
         );",
    )?;
    Ok(())
}

/// 保留最近七天写入的有限数量回执；最旧优先腾出空间，并发写入共用一笔事务。
fn save(
    db: &mut Connection,
    bot: &str,
    room: &str,
    text: &str,
    now: i64,
    check_turn: impl FnOnce() -> Result<()>,
) -> Result<String> {
    let id = uuid::Uuid::now_v7().to_string();
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // 群永久删除也持有当前缓存库的写锁，检查放在锁内才能阻止已清理的群被迟到回执重新写入。
    check_turn()?;
    expire(&tx, now)?;
    tx.execute(
        "DELETE FROM receipts WHERE id IN (SELECT id FROM receipts ORDER BY created DESC, id DESC LIMIT -1 OFFSET ?1)",
        [MAX_RECEIPTS - 1],
    )?;
    tx.execute("INSERT INTO receipts VALUES (?1,?2,?3,?4,?5)", params![id, bot, room, now, text])?;
    tx.commit()?;
    Ok(id)
}

/// 根据当前时钟删除已过期的正文，同时清理 SQLite 释放页里的内容。
fn expire(db: &Connection, now: i64) -> Result<()> {
    db.execute("DELETE FROM receipts WHERE created < ?1", [now - TTL])?;
    Ok(())
}

/// 缓存位于本机数据目录，派工、同步和附件都不包含原文。
fn cache_path() -> std::path::PathBuf {
    crate::service::data_dir().join("tool-receipts")
}

/// Host 启动与长期空闲时都清理已过期原文，不依赖用户再次运行工具。
pub(crate) async fn sweep_loop() {
    let mut timer = tokio::time::interval(Duration::from_mins(15));
    loop {
        timer.tick().await;
        let path = cache_path();
        // 还没有长回执的账户不创建空缓存。
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            if path.join("receipts.sqlite3").try_exists()? {
                expire(&open(&path)?, crate::store::now_ms())?;
            }
            Ok(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!("couldn't expire cached tool receipts");
        }
    }
}

/// 配合已有的群永久删除重试，清理该房间及其已删除成员拥有的回执。
pub(crate) fn purge(ids: &[String]) -> Result<()> {
    let path = cache_path();
    if !path.join("receipts.sqlite3").try_exists()? {
        return Ok(());
    }
    let mut db = open(&path)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for id in ids {
        tx.execute("DELETE FROM receipts WHERE room = ?1 OR bot = ?1", [id])?;
    }
    tx.commit()?;
    Ok(())
}

/// 获取当前可信执行回合。持久暂停先于代理取消，已经暂停的投递无法访问缓存。
fn current_scope(hub: &Hub, bot: &str, group: Option<&str>) -> Result<Scope> {
    use crate::chat::team::Turn;
    let (turn, generation) = hub.team.receipt_turn(bot, group)?;
    let room = if let Turn::Delivery { room, id, .. } = turn {
        let delivery = hub.store.delivery(&id)?.ok_or_else(|| anyhow!("the originating delivery has ended"))?;
        ensure!(
            delivery.status == crate::store::deliveries::Status::Started && !delivery.held_after_pause(),
            "the originating delivery is not running"
        );
        Some(room)
    } else {
        None
    };
    Ok(Scope { room, generation })
}

/// 在现有连接器预检中捕获作用域，不为短回执引入第二次本机请求。
pub(crate) fn connector_scope(hub: &Hub, body: &Value) -> Value {
    let Some(bot) = body["botId"].as_str() else { return Value::Null };
    current_scope(hub, bot, body["groupSession"].as_str())
        .ok()
        .and_then(|scope| serde_json::to_value(scope).ok())
        .unwrap_or(Value::Null)
}

/// 以 Unicode 字符分页，或定位关键词后读取其周围原文。查询与分页使用同一条权限过滤。
fn read(
    db: &Connection,
    bot: &str,
    room: &str,
    id: &str,
    offset: usize,
    query: Option<&str>,
    now: i64,
) -> Result<Value> {
    let (length, start, matched, text): (i64, i64, Option<i64>, String) = db
        .query_row(
            "WITH permitted AS (
                SELECT text, length(text) AS len,
                       CASE WHEN ?7 IS NOT NULL THEN nullif(instr(substr(text, ?1 + 1), ?7), 0) + ?1 - 1 END AS matched
                FROM receipts WHERE id = ?3 AND bot = ?4 AND room = ?5 AND created >= ?6
             )
             SELECT len, CASE WHEN ?7 IS NULL THEN ?1 ELSE max(0, coalesce(matched, ?1) - 180) END, matched,
                    CASE WHEN ?7 IS NULL THEN substr(text, ?1 + 1, ?2)
                         WHEN matched IS NULL THEN ''
                         ELSE substr(text, max(0, matched - 180) + 1, 900) END
             FROM permitted",
            params![i64::try_from(offset)?, i64::try_from(PAGE)?, id, bot, room, now - TTL, query],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("tool receipt not found or expired"))?;
    let length = usize::try_from(length)?;
    ensure!(offset <= length, "offset exceeds the original receipt length");
    if query.is_some() && matched.is_none() {
        return Ok(
            json!({"receiptId":id, "offset":offset, "matchOffset":null, "nextOffset":null, "totalCharacters":length, "text":""}),
        );
    }
    let start = usize::try_from(start)?;
    let end = start + text.chars().count();
    let next = (end < length).then_some(end);
    Ok(
        json!({"receiptId":id, "offset":start, "matchOffset":matched, "nextOffset":next, "totalCharacters":length, "text":text}),
    )
}

/// 处理本机工具桥请求；存档和分页均校验群归属，只记录动作和成功类别。
pub(crate) async fn dispatch(hub: &Arc<Hub>, body: Value) -> Result<Value> {
    let bot = body["botId"].as_str().ok_or_else(|| anyhow!("missing bot identity"))?.to_owned();
    let group = body["groupSession"].as_str().map(str::to_owned);
    let current = current_scope(hub, &bot, group.as_deref())?;
    let op = body["op"].as_str().unwrap_or_default().to_owned();
    if op == "scope" {
        return Ok(serde_json::to_value(current)?);
    }
    let room = current.room.as_deref().unwrap_or_default().to_owned();
    let path = cache_path();
    let mut body = body;
    let data = body["data"].take();
    let now = crate::store::now_ms();
    let result = match op.as_str() {
        "save" => {
            let binding = serde_json::from_value::<Scope>(data["scope"].clone())?;
            ensure!(binding == current, "the originating tool turn has ended");
            let encoded = data["body"].as_str().ok_or_else(|| anyhow!("missing receipt body"))?;
            ensure!(encoded.len() <= MAX_BYTES.div_ceil(3) * 4, "receipt is too large");
            let text = String::from_utf8(STANDARD.decode(encoded)?)?;
            ensure!(eligible(&text), "ineligible receipt");
            let hub = hub.clone();
            // 阻塞写入前再次比较执行代次和删除状态，迟到回执不重建已删群的缓存。
            tokio::task::spawn_blocking(move || {
                let id = save(&mut open(&path)?, &bot, &room, &text, now, || {
                    ensure!(
                        current_scope(&hub, &bot, group.as_deref())? == binding,
                        "the originating tool turn has ended"
                    );
                    ensure!(hub.store.bot(&bot)?.is_some_and(|row| !row.deleted), "the originating bot was deleted");
                    if !room.is_empty() {
                        ensure!(
                            hub.store.bot(&room)?.is_some_and(|row| !row.deleted),
                            "the originating group was deleted"
                        );
                    }
                    Ok(())
                })?;
                Ok(json!({"summary":summarize(&id, &text)}))
            })
            .await?
        }
        "read" => {
            let id = data["receiptId"]
                .as_str()
                .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                .ok_or_else(|| anyhow!("invalid receiptId"))?
                .to_owned();
            let offset = data
                .get("offset")
                .map_or(Some(0), Value::as_u64)
                .and_then(|offset| usize::try_from(offset).ok())
                .filter(|offset| *offset <= MAX_BYTES)
                .ok_or_else(|| anyhow!("invalid offset"))?;
            let query = data
                .get("query")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|text| {
                            !text.is_empty() && text.chars().take(121).count() <= 120 && !text.contains('\0')
                        })
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("invalid query"))
                })
                .transpose()?;
            // 读取和失败都只记录动作，不采集关键词、路径、内容或 Bot 身份。
            let owner = bot.clone();
            let result = tokio::task::spawn_blocking(move || {
                read(&open(&path)?, &owner, &room, &id, offset, query.as_deref(), now)
            })
            .await?;
            ensure!(current_scope(hub, &bot, group.as_deref())? == current, "the originating tool turn has ended");
            result
        }
        _ => return Err(anyhow!("unknown tool receipt operation")),
    };
    crate::analytics::capture(
        hub,
        crate::analytics::Event::ToolReceiptHandled,
        serde_json::Map::from_iter([
            ("action".into(), json!(op)),
            ("outcome".into(), json!(if result.is_ok() { "ok" } else { "unavailable" })),
        ]),
    );
    result
}

#[cfg(test)]
mod tests;
