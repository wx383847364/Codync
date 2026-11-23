use super::str_arg;
use crate::chat::files::{self, SharedFile};
use crate::hub::Hub;
use anyhow::{Result, anyhow, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use std::sync::Arc;

/// 读取消息归属聊天的不可变快照，群作者离开后已发布文件仍归该群。
fn shared(hub: &Hub, body: &Value) -> Result<(std::path::PathBuf, SharedFile)> {
    let entry = hub.store.entry(str_arg(body, "entryId")?).ok_or_else(|| anyhow!("unknown file message"))?;
    ensure!(
        entry.kind == crate::store::EntryKind::Agent.as_str()
            && entry.data["final"] == true
            && entry.data["removed"] != true,
        "not a visible file message"
    );
    let author = entry.data["author"].as_str().ok_or_else(|| anyhow!("missing file author"))?;
    let chat = hub.store.bot(&entry.bot_id)?.filter(|row| !row.deleted).ok_or_else(|| anyhow!("unknown chat"))?;
    ensure!(chat.config.is_group() || entry.bot_id == author, "not a chat file");
    let id = str_arg(body, "fileId")?;
    let meta = entry.data["files"]
        .as_array()
        .and_then(|files| files.iter().find(|meta| meta["id"] == id))
        .ok_or_else(|| anyhow!("file does not belong to this message"))?;
    Ok((files::root(&entry.bot_id), serde_json::from_value(meta.clone())?))
}

/// 沿用已有认证和分页协议，不接收客户端提供的任意文件路径。
pub async fn read(hub: &Arc<Hub>, body: &Value) -> Result<Value> {
    let (root, meta) = shared(hub, body)?;
    let offset = body["offset"].as_u64().ok_or_else(|| anyhow!("offset must be a nonnegative integer"))?;
    let size = meta.size;
    let data = tokio::task::spawn_blocking(move || files::read(&root, &meta, offset)).await??;
    Ok(json!({"data": base64::engine::general_purpose::STANDARD.encode(data), "size": size}))
}

/// 校验群身份后读取用量与文件占用，只有本机调用会收到目录路径。
pub(super) async fn group_usage(hub: &Arc<Hub>, room: &str, local: bool) -> Result<Value> {
    let group = hub
        .store
        .bot(room)?
        .filter(|row| !row.deleted && row.config.is_group())
        .ok_or_else(|| anyhow!("group conversation not found"))?
        .config;
    let hub = hub.clone();
    // 磁盘访问与数据库汇总离开异步线程。远程客户端只接收数值。
    tokio::task::spawn_blocking(move || {
        let mut usage = hub.store.group_token_usage(&group.id)?;
        match files::group_resources(&group) {
            Ok((bytes, directory)) => {
                usage["resourceBytes"] = json!(bytes);
                if local {
                    usage["resourceDirectory"] = json!(directory);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not read group resources");
                usage["resourceBytes"] = Value::Null;
            }
        }
        Ok::<_, anyhow::Error>(usage)
    })
    .await?
}

/// 图片资格检查仅接受已有附件 ID 或消息文件 ID，绝不扫描正文中的路径。
pub async fn info(hub: &Arc<Hub>, body: &Value) -> Result<Value> {
    static READS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let path = if body["entryId"].is_string() {
        let (root, file) = shared(hub, body)?;
        root.join(uuid::Uuid::parse_str(&file.id)?.to_string())
    } else {
        let bot = hub
            .store
            .bot(str_arg(body, "botId")?)?
            .filter(|bot| !bot.deleted)
            .ok_or_else(|| anyhow!("unknown chat"))?;
        crate::chat::uploads::resolve(&crate::chat::uploads::root(&bot.config), str_arg(body, "uploadId")?)?.0
    };
    let permit = READS.acquire().await?;
    let result = tokio::task::spawn_blocking(move || {
        // 请求取消后阻塞任务仍持有许可，防止并发检查超过上限。
        let _permit = permit;
        files::image_dimensions(&path)
    })
    .await??;
    Ok(json!({"width": result.0, "height": result.1}))
}
