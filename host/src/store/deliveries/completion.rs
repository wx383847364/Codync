//! 结果、终态、派生投递与聚合卡片的同事务保存。

use super::{COLS, Delivery, Status, get, insert, row, wire};
use crate::{
    LockExt,
    store::{BotConfig, Entry, EntryKind, Store, now_ms},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

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
        items.push(json!({"deliveryId":delivery.id,"targetBotId":delivery.target_bot_id,"status":delivery.status,"detail":detail}));
    }
    let body = if let Some(source) = &d.source_entry_id {
        c.query_row("SELECT json_extract(data,'$.text') FROM entries WHERE id=?", [source], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or_default()
    } else {
        d.body.clone().unwrap_or_default()
    };
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

impl Store {
    /// 结果、带条件终态和后续 @ 同事务保存；迟到结果只留记录，不复活已取消执行。
    pub fn complete_delivery(
        &self,
        id: &str,
        status: Status,
        text: Option<&str>,
        detail: Option<&str>,
        turn: i64,
    ) -> Result<Vec<Entry>> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let d = get(&tx, id)?.ok_or_else(|| anyhow::anyhow!("unknown delivery"))?;
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
            if changed == 1 && status == Status::Completed {
                let config: Option<String> = tx
                    .query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [&d.room_id], |r| r.get(0))
                    .optional()?;
                if let Some(config) = config {
                    let group: BotConfig = serde_json::from_str(&config)?;
                    let mut members = Vec::new();
                    for id in &group.members {
                        let config: Option<String> = tx
                            .query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [id], |r| r.get(0))
                            .optional()?;
                        if let Some(config) = config {
                            members.push(serde_json::from_str::<BotConfig>(&config)?);
                        }
                    }
                    // 同一执行已用工具派工的目标，最终展示性 @ 不再产生第二份投递。
                    let tasked = tx
                        .prepare("SELECT target_bot_id FROM deliveries WHERE source_execution_id=? AND kind='task'")?
                        .query_map([id], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let targets = crate::chat::mentions::targets(text, &[], &members, None, false);
                    match targets {
                        Ok(targets) => {
                            for target in targets.into_iter().filter(|t| t != &d.target_bot_id && !tasked.contains(t)) {
                                let mut next = Delivery::speak(&d.room_id, &result_id, &target);
                                next.source_kind = "reply".into();
                                next.source_bot_id = Some(d.target_bot_id.clone());
                                next.source_execution_id = Some(d.id.clone());
                                next.batch_root_id.clone_from(&d.batch_root_id);
                                // 轮次和链深仅用于追踪，不能把仍需回复的任务截断。
                                next.round = d.round.saturating_add(1);
                                next.chain_depth = d.chain_depth.saturating_add(1);
                                let next = insert(&tx, &next)?;
                                entries.push(card(&tx, &next)?);
                            }
                        }
                        Err(error) => entries.push(entry(
                            &tx,
                            &format!("delivery-route-error-{id}"),
                            &d.room_id,
                            EntryKind::Notice,
                            turn,
                            &json!({"text":error.to_string(),"style":"error","deliveryId":id}),
                        )?),
                    }
                }
            }
        }
        entries.push(card(&tx, &d)?);
        tx.commit()?;
        Ok(entries)
    }
}
