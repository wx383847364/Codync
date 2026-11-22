//! 当前真实群执行的可分页原文读取；返回预算不改变持久原文。

use crate::hub::Hub;
use crate::store::group_context::HistoryRows;
use anyhow::{Result, bail, ensure};
use codync_mem::tokens::estimate;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::sync::Arc;

const RESPONSE_TOKENS: usize = 2_048;
const RESPONSE_BYTES: usize = 8_192;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Read {
    context_delivery_id: String,
    scope: Option<String>,
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
        if let Some(scope) = &request.scope {
            ensure!(scope == "previousStage", "Unknown history scope");
            let result = previous_stage(&hub, &bot, &request);
            // 只记录读取类型和结果，搜索词、资料 ID 与正文都留在 Host。
            crate::analytics::capture(&hub, crate::analytics::Event::PreviousStageReferenceRead, serde_json::Map::from_iter([
                ("kind".into(), json!(if request.kind.as_deref() == Some("handoff") { "handoff" } else { "messages" })),
                ("outcome".into(), json!(if result.is_ok() { "read" } else { "failed" })),
                ("category".into(), json!(result.is_err().then_some("reference"))),
            ]));
            return result;
        }
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
            HistoryRows::Messages(entries)=>entries.iter().map(message_result).collect::<Vec<_>>(),
            HistoryRows::Deliveries(rows)=>rows.into_iter().map(|d|json!({"id":d.id,"roomSeq":d.room_seq,"targetBotId":d.target_bot_id,"batchRootId":d.batch_root_id,"kind":d.kind,"status":d.status,"attempt":d.attempt,"parentId":d.parent_id,"sourceEntryId":d.source_entry_id,"readCompleteWith":"deliveryId"})).collect(),
        };
        list_page(&request,rows,limit)
    }).await?
}

/// 按需查询上一阶段聊天或交接，旧资料只作为参考。
fn previous_stage(hub: &Hub, bot: &str, request: &Read) -> Result<Value> {
    let kind = request.kind.as_deref().unwrap_or("messages");
    ensure!(request.delivery_id.is_none(), "Previous-stage access supports visible messages and accepted handoff only");
    if kind == "handoff" {
        ensure!(
            request.entry_id.is_none() && request.from.is_none() && request.query.is_none() && request.part.is_none(),
            "Handoff supports versioned pagination only"
        );
        let (room, stage) = hub.store.previous_stage_handoff(&request.context_delivery_id, bot)?;
        let Some(file) = stage.incoming else {
            return Ok(json!({"scope":"previousStage","referenceOnly":true,"available":false}));
        };
        let body = crate::chat::files::read_markdown(&room, &file)?;
        let mut evidence = String::new();
        for file in stage.incoming_files {
            let path = crate::chat::files::verify_shared(&room, &file)?;
            let _ =
                writeln!(evidence, "\nEvidence file {} at {} (SHA-256 {}).", file.name, path.display(), file.sha256);
        }
        material_page(request, &format!("{body}{evidence}"), json!({"kind":"handoff","name":file.name}), "text")
    } else {
        ensure!(kind == "messages", "Previous-stage access supports visible messages and accepted handoff only");
        if let Some(id) = &request.entry_id {
            let entry = hub.store.read_previous_stage_message(&request.context_delivery_id, bot, id)?;
            // 跨阶段回查仅返回公开正文和作者，不暴露底层附件凭据或内部执行元数据。
            ensure!(
                request.part.as_deref().is_none_or(|part| part == "text"),
                "Previous-stage message reads support text only"
            );
            material_page(
                request,
                entry.data["text"].as_str().unwrap_or_default(),
                json!({"author":entry.data["author"],"kind":entry.kind}),
                "text",
            )
        } else {
            ensure!(request.offset == 0 && request.part.is_none(), "Read a message by entryId before paging its text");
            let terms: Vec<_> =
                request.query.as_deref().unwrap_or_default().split_whitespace().take(8).map(str::to_owned).collect();
            let limit = request.limit.unwrap_or(10).clamp(1, 30);
            let rows = hub.store.search_previous_stage_messages(
                &request.context_delivery_id,
                bot,
                &terms,
                request.from.as_deref(),
                i64::try_from(limit + 1)?,
            )?;
            list_page(request, rows.iter().map(message_result).collect(), limit)
        }
    }
}

/// 本群和来源群共享有界摘录格式，原文仍需单独读取。
fn message_result(entry: &crate::store::Entry) -> Value {
    let raw = if entry.kind == "plan" {
        entry.data["entries"].to_string()
    } else {
        entry.data["text"].as_str().unwrap_or_default().to_owned()
    };
    json!({"id":entry.id,"seq":entry.seq,"kind":entry.kind,"author":entry.data["author"],
        "excerpt":super::context::excerpt(&raw,256,1_024),"readCompleteWith":"entryId"})
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
        let mut answer = json!({"contextDeliveryId":request.context_delivery_id,"entryId":request.entry_id,"deliveryId":request.delivery_id,"part":part,"version":version,"offset":request.offset,"text":chunk,"nextOffset":(end<total).then_some(end),"totalCharacters":total,"complete":end==total,"metadata":meta});
        mark_reference(request, &mut answer);
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
        if !fits(&list_result(request, &shown, Some(&id)))? {
            shown.pop();
            next = Some(id);
            break;
        }
    }
    ensure!(!shown.is_empty() || next.is_none(), "one row exceeds the response budget; read it directly by ID");
    Ok(list_result(request, &shown, next.as_deref()))
}

/// 同时用于测量和构造结果，引用标记也计入响应预算。
fn list_result(request: &Read, rows: &[Value], next: Option<&str>) -> Value {
    let mut result = json!({"contextDeliveryId":request.context_delivery_id,
        "kind":request.kind.as_deref().unwrap_or("messages"),"results":rows,"next":next,"estimatesOnly":true});
    mark_reference(request, &mut result);
    result
}

/// 统一标记跨阶段参考，保留本群检索原有响应结构。
fn mark_reference(request: &Read, result: &mut Value) {
    if request.scope.as_deref() == Some("previousStage") {
        result["scope"] = json!("previousStage");
        result["referenceOnly"] = json!(true);
    }
}

/// 计量是估算而非 provider tokenizer，字节上限同时防止估算偏低时无限放大响应。
fn fits(value: &Value) -> Result<bool> {
    let text = serde_json::to_string(value)?;
    Ok(text.len() <= RESPONSE_BYTES && estimate(&text) <= RESPONSE_TOKENS)
}
