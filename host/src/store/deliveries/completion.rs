//! 结果、终态、派生投递与聚合卡片的同事务保存。

use super::{COLS, Delivery, REPLY_MENTION, REPLY_NOTICE, Status, TOOL_RETURN, eligible, get, insert, row, wire};
use crate::{
    LockExt,
    store::{BotConfig, Entry, EntryKind, Store, now_ms},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

/// 同一执行接收引导后的结果，随原执行一起落库。
#[derive(Clone, Copy)]
pub enum Guidance {
    Joined,
    Deferred,
    Uncertain,
}

/// 在调用方事务内保存 entry；确定 ID 允许完成保存重试而不重复结果。
pub(super) fn entry(c: &Connection, id: &str, room: &str, kind: EntryKind, turn: i64, data: &Value) -> Result<Entry> {
    super::super::entries::write_entry(c, id, &crate::store::Lane::main(room), kind, turn, data)
}

/// 同一来源事件和 attempt 使用稳定 ID，刷新时只写一次。
pub(super) fn card_key(d: &Delivery) -> String {
    let source = d.source_entry_id.as_deref().or(d.source_execution_id.as_deref()).unwrap_or(&d.id);
    format!("delivery-card-{source}-{}", d.attempt)
}

/// 一次事务只刷新实际变更来源事件，避免对历史卡片重复增 rev。
pub(super) fn cards(c: &Connection, rows: Vec<Delivery>) -> Result<Vec<Entry>> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter().filter(|d| seen.insert(card_key(d))).map(|d| card(c, &d)).collect()
}

/// 来源事件和 attempt 共享一张卡；工具来源按执行聚合，重试保留新旧卡片。
pub(super) fn card(c: &Connection, d: &Delivery) -> Result<Entry> {
    let source = d.source_entry_id.as_deref().or(d.source_execution_id.as_deref()).unwrap_or(&d.id);
    let id = card_key(d);
    let mut st = c.prepare(&format!("SELECT {COLS} FROM deliveries WHERE room_id=?1 AND attempt=?2 AND (source_entry_id=?3 OR (source_entry_id IS NULL AND source_execution_id=?3) OR id=?3) ORDER BY room_seq"))?;
    let deliveries = st.query_map(params![d.room_id, d.attempt, source], row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let source_data = d
        .source_entry_id
        .as_deref()
        .map(|id| c.query_row("SELECT data FROM entries WHERE id=?", [id], |r| r.get::<_, String>(0)).optional())
        .transpose()?
        .flatten()
        .map(|data| serde_json::from_str::<Value>(&data))
        .transpose()?
        .unwrap_or_default();
    let mut lines = Vec::new();
    let mut items = Vec::new();
    for delivery in &deliveries {
        let name: Option<String> = c
            .query_row("SELECT json_extract(config,'$.name') FROM bots WHERE id=?", [&delivery.target_bot_id], |r| {
                r.get(0)
            })
            .optional()?;
        let detail = delivery.last_error.as_deref().unwrap_or_default();
        lines.push(format!(
            "{}: {} {}",
            name.as_deref().unwrap_or(&delivery.target_bot_id),
            wire(&delivery.status),
            detail
        ));
        items.push(json!({"deliveryId":delivery.id,"targetBotId":delivery.target_bot_id,"status":delivery.status,"detail":detail,"canGuide": delivery.status == Status::Queued && direct_user_mention(delivery, &source_data),"groupHandoff":super::handoff::projection(c,delivery)?}));
    }
    let body = source_data["text"].as_str().or(d.body.as_deref()).unwrap_or_default();
    let turn = c.query_row("SELECT COALESCE(MAX(turn),0) FROM entries WHERE bot_id=?", [&d.room_id], |r| r.get(0))?;
    entry(
        c,
        &id,
        &d.room_id,
        EntryKind::Notice,
        turn,
        &json!({"deliveryId":d.id,"deliveryAttempt":d.attempt,
        "deliveryBody":body,"deliveryRows":items,"parentDeliveryId":d.parent_id,"text":format!("{body}\n{}",lines.join("\n")),"style":"info"}),
    )
}

/// 仅允许原始用户消息中明确 @ 的投递加入当前执行。
fn direct_user_mention(delivery: &Delivery, source: &Value) -> bool {
    delivery.source_kind == "user"
        && delivery.attempt == 1
        && source["directMentions"].as_array().is_some_and(|ids| ids.iter().any(|id| *id == delivery.target_bot_id))
}

impl Store {
    /// 在发送 ACP 之前持久登记引导；Stop 或重启后按既有 started 边界处理。
    pub fn start_guidance(&self, id: &str, active_id: &str, bot: &str) -> Result<(Entry, Vec<Entry>)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let delivery = get(&tx, id)?
            .filter(|d| d.target_bot_id == bot && d.status == Status::Queued)
            .ok_or_else(|| anyhow::anyhow!("这条消息已不在等待执行"))?;
        anyhow::ensure!(
            get(&tx, active_id)?.is_some_and(|d| d.target_bot_id == bot
                && d.room_id == delivery.room_id
                && d.status == Status::Started)
                && eligible(&tx, &delivery.room_id, bot, None)?,
            "成员的当前回合已结束或已切换"
        );
        let source = tx
            .query_row(
                &format!(
                    "SELECT {} FROM entries WHERE id=? AND bot_id=? AND kind='user'",
                    super::super::entries::ENTRY_COLS
                ),
                params![delivery.source_entry_id, delivery.room_id],
                super::super::entries::row_entry,
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("原始消息已不可读"))?;
        anyhow::ensure!(direct_user_mention(&delivery, &source.data), "引导需要直接 @ 这位成员");
        tx.execute(
            "UPDATE deliveries SET status='started',started_at=?2,last_error='引导当前对话' WHERE id=?1",
            params![id, now_ms()],
        )?;
        let cards = vec![card(&tx, &delivery)?];
        tx.commit()?;
        Ok((source, cards))
    }

    /// 结果、带条件终态和后续 @ 同事务保存；迟到结果只留记录，不复活已取消执行。
    pub fn complete_delivery(
        &self,
        id: &str,
        status: Status,
        text: Option<&str>,
        detail: Option<&str>,
        turn: i64,
        guidance: &[(String, Guidance)],
    ) -> Result<Vec<Entry>> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let d = get(&tx, id)?.ok_or_else(|| anyhow::anyhow!("unknown delivery"))?;
        // 暂停后的旧回合可能在继续请求之后才退出。它的片段已保存在执行记录中，
        // 不能写入共享的最终结果 ID，也不能触发派工；同一成员会等它退出才重新领取。
        if d.held_after_pause() {
            // 原回合取消后，引导不能永远停留在 started；已接受或结果不明的引导需要人工检查，
            // 只有明确被执行器拒绝的引导能恢复为普通排队消息。
            let mut changed = Vec::new();
            for (guide, outcome) in guidance {
                let queued = matches!(outcome, Guidance::Deferred);
                let next = if queued { Status::Queued } else { Status::Interrupted };
                let detail = if queued {
                    "等待下一回合"
                } else {
                    "原任务已暂停，引导已中断，请检查进度后重试"
                };
                let count = tx.execute(
                    "UPDATE deliveries SET status=?2,finished_at=?3,started_at=CASE WHEN ?4 THEN NULL ELSE started_at END,last_error=?5 WHERE id=?1 AND status='started'",
                    params![guide, wire(&next), (!queued).then(now_ms), queued, detail],
                )?;
                if count == 1
                    && let Some(delivery) = get(&tx, guide)?
                {
                    changed.push(delivery);
                }
            }
            let entries = cards(&tx, changed)?;
            tx.commit()?;
            return Ok(entries);
        }
        let changed = tx.execute("UPDATE deliveries SET status=?2,finished_at=?3,last_error=?4 WHERE id=?1 AND status IN ('claimed','started')",
            params![id,wire(&status),now_ms(),detail])?;
        let deleted: bool =
            tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM bots WHERE id=? AND deleted=0)", [&d.room_id], |r| r.get(0))?;
        if deleted {
            tx.commit()?;
            return Ok(vec![]);
        }
        if changed == 0
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM entries WHERE id=?)",
                [format!("delivery-result-{id}")],
                |r| r.get::<_, bool>(0),
            )?
        {
            tx.commit()?;
            return Ok(vec![]);
        }
        let result_id = format!("delivery-result-{id}");
        let mut entries = Vec::new();
        if changed == 1 {
            entries.extend(super::handoff::discard(&tx, &d)?);
        }
        if let Some(text) = text {
            let mut data = json!({"text":text,"author":d.target_bot_id,
                "deliveryId":id,"final":changed==1 && status==Status::Completed,"late":changed==0});
            // 最终回复在完成事务内重新检查通话是否已结束，迟到结果不会重入已关闭通话。
            let source = d.batch_root_id.as_deref().or(d.source_entry_id.as_deref());
            let call: Option<String> = tx.query_row(
                "SELECT json_extract(data,'$.call') FROM entries WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM kv WHERE k='call.ended.' || json_extract(data,'$.call'))",
                [source], |r| r.get(0)).optional()?.flatten();
            if let Some(call) = call {
                data["call"] = call.into();
            }
            let result = entry(&tx, &result_id, &d.room_id, EntryKind::Agent, turn, &data)?;
            entries.push(result);
        }
        if changed == 1 && status == Status::Completed {
            // 只从本次投递实际发布的文件回传，文件名中的 @ 不是成员提及。
            let source = match text {
                Some(_) => Some(result_id),
                None if d.source_kind == TOOL_RETURN => tx.query_row(
                    "SELECT id FROM entries WHERE bot_id=?1 AND kind='agent' AND json_extract(data,'$.deliveryId')=?2 AND json_array_length(data,'$.files')>0 ORDER BY seq DESC LIMIT 1",
                    params![d.room_id, d.id], |r| r.get(0),
                ).optional()?,
                None => None,
            };
            if let Some(source) = source {
                entries.extend(route_result(&tx, &d, &source, text, turn)?);
            }
        }
        // 引导与原执行共用结果，但每条来源消息保留自己的投递记录。
        let mut changed_rows = vec![d];
        for (id, outcome) in guidance {
            let (next, detail) = match outcome {
                Guidance::Joined => (status, "已引导当前对话"),
                Guidance::Deferred => (Status::Queued, "等待下一回合"),
                Guidance::Uncertain => (Status::Interrupted, "引导结果不确定，请检查后重试"),
            };
            let queued = next == Status::Queued;
            let count = tx.execute(
                "UPDATE deliveries SET status=?2,finished_at=?3,started_at=CASE WHEN ?4 THEN NULL ELSE started_at END,last_error=?5 WHERE id=?1 AND status='started'",
                params![id, wire(&next), (!queued).then(now_ms), queued, detail],
            )?;
            if count == 1
                && let Some(delivery) = get(&tx, id)?
            {
                changed_rows.push(delivery);
            }
        }
        entries.extend(cards(&tx, changed_rows)?);
        tx.commit()?;
        Ok(entries)
    }
}

/// 只派发正文里的有效提及和明确要求的回传，目标、结果和终态共用事务。
fn route_result(c: &Connection, d: &Delivery, source: &str, text: Option<&str>, turn: i64) -> Result<Vec<Entry>> {
    let config: String = c.query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [&d.room_id], |r| r.get(0))?;
    let group: BotConfig = serde_json::from_str(&config)?;
    // 已关闭时仍可保存迟到记录，但不能在同一事务派生新的工作。
    if group.stage.as_ref().is_some_and(crate::store::StageGroup::read_only) {
        return Ok(vec![]);
    }
    let mut members = Vec::new();
    for id in &group.members {
        let config: Option<String> =
            c.query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [id], |r| r.get(0)).optional()?;
        if let Some(config) = config {
            members.push(serde_json::from_str::<BotConfig>(&config)?);
        }
    }
    let mut entries = Vec::new();
    // 同一执行已用工具派工的目标，最终展示性 @ 不再产生第二份投递。
    let tasked = c
        .prepare("SELECT target_bot_id FROM deliveries WHERE source_execution_id=? AND kind='task'")?
        .query_map([&d.id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut targets, direct) = match crate::chat::mentions::route(text.unwrap_or_default(), &[], &members, None) {
        Ok((mut targets, direct)) => {
            targets.retain(|target| target != &d.target_bot_id && !tasked.contains(target));
            (targets, direct)
        }
        Err(error) => {
            entries.push(entry(
                c,
                &format!("delivery-route-error-{}", d.id),
                &d.room_id,
                EntryKind::Notice,
                turn,
                &json!({"text":error.to_string(),"style":"error","deliveryId":d.id}),
            )?);
            (Vec::new(), Vec::new())
        }
    };
    // 明确请求的结果只回给原发起人一次，展示性的 @ 复用同一投递。
    if group.stage.is_some()
        && d.source_kind == TOOL_RETURN
        && let Some(sender) = d.source_bot_id.as_ref()
        && members.iter().any(|member| &member.id == sender)
        && !targets.contains(sender)
    {
        targets.push(sender.clone());
    }
    for target in targets {
        let mut next = Delivery::speak(&d.room_id, source, &target);
        // 自动结果回传和广播也创建回合，但没有直接点名的回应要求。
        next.source_kind = if direct.contains(&target) { REPLY_MENTION } else { REPLY_NOTICE }.into();
        next.source_bot_id = Some(d.target_bot_id.clone());
        next.source_execution_id = Some(d.id.clone());
        next.batch_root_id.clone_from(&d.batch_root_id);
        // 轮次和链深仅用于追踪，不能把仍需回复的任务截断。
        next.round = d.round.saturating_add(1);
        next.chain_depth = d.chain_depth.saturating_add(1);
        let next = insert(c, &next)?;
        entries.push(card(c, &next)?);
    }
    Ok(entries)
}
