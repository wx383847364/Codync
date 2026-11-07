//! 当前真实群执行的可分页原文读取；返回预算不改变持久原文。

use crate::hub::Hub;
use crate::store::group_context::HistoryRows;
use anyhow::{Result, bail, ensure};
use codync_mem::tokens::estimate;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

const RESPONSE_TOKENS: usize = 2_048;
const RESPONSE_BYTES: usize = 8_192;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Read {
    context_delivery_id: String,
    entry_id: Option<String>,
    delivery_id: Option<String>,
    #[serde(default)]
    offset: usize,
    version: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    from: Option<String>,
}

/// 模型只能提供与可信执行相等的上下文 ID，不能选择另一个群或私聊。
pub(crate) async fn read(hub: &Arc<Hub>, bot: &str, current: &str, args: &Value) -> Result<Value> {
    let request: Read = serde_json::from_value(args.clone())?;
    ensure!(request.context_delivery_id == current, "contextDeliveryId does not match the current group execution");
    ensure!(!(request.entry_id.is_some() && request.delivery_id.is_some()), "choose entryId or deliveryId");
    let (hub, bot) = (hub.clone(), bot.to_owned());
    tokio::task::spawn_blocking(move || -> Result<Value> {
        if request.entry_id.is_some() || request.delivery_id.is_some() {
            let (body,metadata)=hub.store.group_material(&request.context_delivery_id,&bot,request.entry_id.as_deref(),request.delivery_id.as_deref())?;
            let part=request.part.as_deref().unwrap_or("text");
            let text=match part { "text"=>body, "metadata"=>serde_json::to_string(&metadata)?, _=>bail!("part must be text or metadata") };
            return material_page(&request,&text,metadata,part);
        }
        ensure!(request.offset==0 && request.part.is_none(),"offset/part require entryId or deliveryId");
        let kind=request.kind.as_deref().unwrap_or("messages");
        let terms=request.query.as_deref().unwrap_or_default().split_whitespace().take(8).map(str::to_owned).collect::<Vec<_>>();
        ensure!(kind=="messages" || terms.is_empty(),"query is supported for kind=messages; read plan/task originals by ID");
        let limit=request.limit.unwrap_or(10).clamp(1,30);
        let rows=hub.store.group_history(&request.context_delivery_id,&bot,kind,&terms,request.from.as_deref(),i64::try_from(limit+1)?)?;
        let rows=match rows {
            HistoryRows::Messages(entries)=>entries.into_iter().map(|e| {
                let raw=if e.kind=="plan" { e.data["entries"].to_string() } else {e.data["text"].as_str().unwrap_or_default().to_owned()};
                json!({"id":e.id,"seq":e.seq,"kind":e.kind,"author":e.data["author"],"excerpt":super::context::excerpt(&raw,256,1_024),"readCompleteWith":"entryId"})
            }).collect::<Vec<_>>(),
            HistoryRows::Deliveries(rows)=>rows.into_iter().map(|d|json!({"id":d.id,"roomSeq":d.room_seq,"targetBotId":d.target_bot_id,"batchRootId":d.batch_root_id,"kind":d.kind,"status":d.status,"attempt":d.attempt,"parentId":d.parent_id,"sourceEntryId":d.source_entry_id,"readCompleteWith":"deliveryId"})).collect(),
        };
        list_page(&request,rows,limit)
    }).await?
}

/// 完整正文和元数据分别按 Unicode 标量分页，响应明确声明是否还有未读取部分。
fn material_page(request: &Read, text: &str, metadata: Value, part: &str) -> Result<Value> {
    // 每页重新验证权限，同时校验完整材料版本；计划改写后必须从头读，不能拼接混合版本。
    let version = crate::chat::files::digest_hex(&Sha256::digest(format!("{part}\0{text}\0{metadata}")));
    ensure!(
        request.offset == 0 || request.version.as_deref() == Some(version.as_str()),
        "material changed or version missing; restart at offset=0 and use the returned version on each next page"
    );
    let total = text.chars().count();
    ensure!(request.offset <= total, "offset is outside the original material");
    let limit = request.limit.unwrap_or(1_024).clamp(1, 4_096);
    let mut length = (total - request.offset).min(limit);
    // 大型附件清单或长引用也不能偷偷截断，提供独立 metadata 分页入口。
    let meta = if serde_json::to_string(&metadata)?.len() <= RESPONSE_BYTES / 4
        && estimate(&metadata.to_string()) <= RESPONSE_TOKENS / 4
    {
        metadata
    } else {
        json!({"notInline":true,"readWithPart":"metadata"})
    };
    loop {
        let chunk = text.chars().skip(request.offset).take(length).collect::<String>();
        let end = request.offset + length;
        let answer = json!({"contextDeliveryId":request.context_delivery_id,"entryId":request.entry_id,"deliveryId":request.delivery_id,"part":part,"version":version,"offset":request.offset,"text":chunk,"nextOffset":(end<total).then_some(end),"totalCharacters":total,"complete":end==total,"metadata":meta});
        if fits(&answer)? {
            return Ok(answer);
        }
        ensure!(length > 1, "material identity metadata exceeds the response budget");
        length /= 2;
    }
}

/// 列表预算不足时 next 指向第一条未显示记录，不能跳过被预算裁掉的待办。
fn list_page(request: &Read, rows: Vec<Value>, limit: usize) -> Result<Value> {
    let mut shown = Vec::new();
    let mut next = None;
    for row in rows {
        let id = row["id"].as_str().unwrap_or_default().to_owned();
        if shown.len() >= limit {
            next = Some(id);
            break;
        }
        shown.push(row);
        if !fits(
            &json!({"contextDeliveryId":request.context_delivery_id,"kind":request.kind.as_deref().unwrap_or("messages"),"results":shown,"next":id,"estimatesOnly":true}),
        )? {
            shown.pop();
            next = Some(id);
            break;
        }
    }
    ensure!(!shown.is_empty() || next.is_none(), "one row exceeds the response budget; read it directly by ID");
    Ok(
        json!({"contextDeliveryId":request.context_delivery_id,"kind":request.kind.as_deref().unwrap_or("messages"),"results":shown,"next":next,"estimatesOnly":true}),
    )
}

/// 计量是估算而非 provider tokenizer，字节上限同时防止估算偏低时无限放大响应。
fn fits(value: &Value) -> Result<bool> {
    let text = serde_json::to_string(value)?;
    Ok(text.len() <= RESPONSE_BYTES && estimate(&text) <= RESPONSE_TOKENS)
}
