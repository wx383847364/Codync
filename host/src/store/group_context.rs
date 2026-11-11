//! 群上下文的同事务只读快照；原文、用户修订、投递和计划有一致的截止序号。

use super::{BotConfig, Entry, Store, deliveries, entries};
use crate::LockExt;
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

const UNRESOLVED: &str = "status IN ('queued','claimed','started') OR ((status IN ('failed','interrupted') OR (kind='task' AND status='noReply')) AND NOT EXISTS(SELECT 1 FROM deliveries child WHERE child.parent_id=deliveries.id))";
const VISIBLE: &str = "COALESCE(json_extract(data,'$.removed'),0)=0 AND (kind='user' OR (kind='agent' AND json_extract(data,'$.final')=1))";

pub(crate) struct Snapshot {
    pub delivery: deliveries::Delivery,
    pub room: BotConfig,
    pub members: Vec<BotConfig>,
    pub cutoff: i64,
    pub root: Option<Entry>,
    pub source: Option<Entry>,
    pub users: Vec<Entry>,
    pub quotes: Vec<Entry>,
    pub pending: Vec<deliveries::Delivery>,
    pub plans: Vec<Entry>,
    pub recent: Vec<Entry>,
}

/// 只允许当前群的消息，隐藏 trace 不因为提供了 ID 就变成可读历史。
fn message(c: &Connection, room: &str, id: &str) -> Result<Entry> {
    c.query_row(
        &format!("SELECT {} FROM entries WHERE id=?1 AND bot_id=?2 AND ({VISIBLE})", entries::ENTRY_COLS),
        params![id, room],
        entries::row_entry,
    )
    .optional()?
    .ok_or_else(|| anyhow!("no visible message {id} in this group"))
}

/// 目标和来源成员资格在同一快照校验；检索还必须对应已提交 started 的真实执行。
pub(super) fn authorized(c: &Connection, id: &str, bot: &str, started: bool) -> Result<deliveries::Delivery> {
    let d = deliveries::get(c, id)?.ok_or_else(|| anyhow!("unknown delivery"))?;
    ensure!(d.target_bot_id == bot, "delivery belongs to another member");
    ensure!(deliveries::eligible(c, &d.room_id, bot, d.source_bot_id.as_deref())?, "成员已不在群内");
    if started {
        ensure!(d.status == deliveries::Status::Started, "来源投递已结束");
    }
    Ok(d)
}

impl Store {
    /// 只读变化指纹避免周期补查反复重建同一个无法装入的任务包。
    pub(crate) fn group_context_revision(&self, room: &str) -> Result<i64> {
        Ok(self
            .db
            .locked()
            .query_row("SELECT COALESCE(MAX(rev),0) FROM entries WHERE bot_id=?", [room], |r| r.get(0))?)
    }

    /// 保护材料与进展来自同一 SQLite 读快照；不写“已读”水位、不消费待办。
    pub(crate) fn group_context(&self, id: &str, bot: &str) -> Result<Snapshot> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let d = authorized(&tx, id, bot, false)?;
        ensure!(
            matches!(d.status, deliveries::Status::Queued | deliveries::Status::Claimed | deliveries::Status::Started),
            "投递已结束"
        );
        let room_config: String =
            tx.query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [&d.room_id], |r| r.get(0))?;
        let room: BotConfig = serde_json::from_str(&room_config)?;
        let mut members = Vec::new();
        for member in &room.members {
            let cfg: Option<String> =
                tx.query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [member], |r| r.get(0)).optional()?;
            if let Some(cfg) = cfg {
                members.push(serde_json::from_str(&cfg)?);
            }
        }
        let cutoff =
            tx.query_row("SELECT COALESCE(MAX(seq),0) FROM entries WHERE bot_id=?", [&d.room_id], |r| r.get(0))?;
        let original = d.batch_root_id.as_deref().map(|id| message(&tx, &d.room_id, id)).transpose()?;
        if let Some(entry) = &original {
            ensure!(entry.kind == "user", "任务根必须是本群用户原文");
        }
        let source = d.source_entry_id.as_deref().map(|id| message(&tx, &d.room_id, id)).transpose()?;
        let from = original.as_ref().map_or(0, |e| e.seq);
        // 后续用户消息原样保留，但不把独立新请求自动改绑到旧任务或变成新授权。
        let users = tx.prepare(&format!("SELECT {} FROM entries WHERE bot_id=?1 AND seq>=?2 AND seq<=?3 AND kind='user' AND COALESCE(json_extract(data,'$.removed'),0)=0 ORDER BY seq", entries::ENTRY_COLS))?
            .query_map(params![d.room_id, from, cutoff], entries::row_entry)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut quotes = Vec::new();
        let mut seen = HashSet::new();
        for e in original.iter().chain(source.iter()).chain(users.iter()) {
            if let Some(id) = e.data["replyTo"]["id"].as_str()
                && seen.insert(id.to_owned())
            {
                // 已删除引用仍保留原消息中的引用快照，不能借引用越权读取其他聊天。
                if let Some(e) = tx
                    .query_row(
                        &format!(
                            "SELECT {} FROM entries WHERE id=?1 AND bot_id=?2 AND ({VISIBLE})",
                            entries::ENTRY_COLS
                        ),
                        params![id, d.room_id],
                        entries::row_entry,
                    )
                    .optional()?
                {
                    quotes.push(e);
                }
            }
        }
        // 成功重试后的旧失败不是新待办；speak 的正常 pass 也不要求人工处理。
        let pending = tx
            .prepare(&format!(
                "SELECT {} FROM deliveries WHERE room_id=?1 AND ({UNRESOLVED}) ORDER BY room_seq",
                deliveries::COLS
            ))?
            .query_map([&d.room_id], deliveries::row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // 同一计划按 entry ID 更新；另一个投递的新计划不能覆盖此成员尚未完成的旧待办。
        let plans = tx.prepare(&format!("SELECT {} FROM entries WHERE bot_id=?1 AND kind='plan' AND seq>=?2 AND seq<=?3 AND (json_extract(data,'$.deliveryId') IS NULL OR EXISTS(SELECT 1 FROM deliveries d WHERE d.id=json_extract(entries.data,'$.deliveryId') AND d.batch_root_id=?4)) ORDER BY seq", entries::ENTRY_COLS))?
            .query_map(params![d.room_id,from,cutoff,d.batch_root_id],entries::row_entry)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut recent = tx
            .prepare(&format!(
                "SELECT {} FROM entries WHERE bot_id=?1 AND seq<=?2 AND ({VISIBLE}) ORDER BY seq DESC LIMIT 120",
                entries::ENTRY_COLS
            ))?
            .query_map(params![d.room_id, cutoff], entries::row_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        recent.reverse();
        // 进展的任务根来自持久投递，不能把另一独立请求的回复当成本任务已处理。
        for entry in &mut recent {
            if let Some(id) = entry.data["deliveryId"].as_str()
                && let Some(delivery) = deliveries::get(&tx, id)?
                && delivery.room_id == d.room_id
            {
                entry.data["contextBatchRootId"] = serde_json::to_value(delivery.batch_root_id)?;
            }
        }
        tx.commit()?;
        Ok(Snapshot {
            delivery: d,
            room,
            members,
            cutoff,
            root: original,
            source,
            users,
            quotes,
            pending,
            plans,
            recent,
        })
    }
}

/// 分页读取结果保留结构化身份，正文的响应预算在协议层应用。
pub(crate) enum HistoryRows {
    Messages(Vec<Entry>),
    Deliveries(Vec<deliveries::Delivery>),
}

impl Store {
    /// 读取原文前逐次校验当前真实执行及成员资格，不能用模型参数指定另一个群。
    pub(crate) fn group_material(
        &self,
        context: &str,
        bot: &str,
        entry: Option<&str>,
        delivery: Option<&str>,
    ) -> Result<(String, serde_json::Value)> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let scope = authorized(&tx, context, bot, true)?;
        let result = if let Some(id) = delivery {
            let d = deliveries::get(&tx, id)?.ok_or_else(|| anyhow!("unknown delivery"))?;
            ensure!(d.room_id == scope.room_id, "delivery belongs to another group");
            if let Some(body) = d.body {
                (
                    body,
                    serde_json::json!({"kind":d.kind,"status":d.status,"targetBotId":d.target_bot_id,"batchRootId":d.batch_root_id}),
                )
            } else {
                let e = message(
                    &tx,
                    &scope.room_id,
                    d.source_entry_id.as_deref().ok_or_else(|| anyhow!("original source unavailable"))?,
                )?;
                (e.data["text"].as_str().unwrap_or_default().to_owned(), material_metadata(&e))
            }
        } else {
            let id = entry.ok_or_else(|| anyhow!("entryId or deliveryId is required"))?;
            let e = tx
                .query_row(
                    &format!(
                        "SELECT {} FROM entries WHERE id=?1 AND bot_id=?2 AND (({VISIBLE}) OR kind='plan')",
                        entries::ENTRY_COLS
                    ),
                    params![id, scope.room_id],
                    entries::row_entry,
                )
                .optional()?
                .ok_or_else(|| anyhow!("no visible material {id} in this group"))?;
            let text = if e.kind == "plan" {
                serde_json::to_string(&e.data["entries"])?
            } else {
                e.data["text"].as_str().unwrap_or_default().to_owned()
            };
            (text, material_metadata(&e))
        };
        tx.commit()?;
        Ok(result)
    }

    /// 搜索和游标验证使用同一权限快照；计划、任务和消息的游标不能跨群使用。
    pub(crate) fn group_history(
        &self,
        context: &str,
        bot: &str,
        kind: &str,
        terms: &[String],
        from: Option<&str>,
        limit: i64,
    ) -> Result<HistoryRows> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        let scope = authorized(&tx, context, bot, true)?;
        let result = match kind {
            "messages" => {
                let seq = from.map(|id| message(&tx, &scope.room_id, id).map(|e| e.seq)).transpose()?;
                HistoryRows::Messages(entries::search_messages_on(&tx, &scope.room_id, terms, seq, limit)?)
            }
            "plans" => {
                let seq = if let Some(id) = from {
                    Some(
                        tx.query_row(
                            "SELECT seq FROM entries WHERE id=?1 AND bot_id=?2 AND kind='plan'",
                            params![id, scope.room_id],
                            |r| r.get::<_, i64>(0),
                        )
                        .optional()?
                        .ok_or_else(|| anyhow!("plan cursor belongs to another scope"))?,
                    )
                } else {
                    None
                };
                let rows = tx
                    .prepare(&format!(
                        "SELECT {} FROM entries WHERE bot_id=?1 AND kind='plan' AND seq<=?2 ORDER BY seq DESC LIMIT ?3",
                        entries::ENTRY_COLS
                    ))?
                    .query_map(params![scope.room_id, seq.unwrap_or(i64::MAX), limit], entries::row_entry)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                HistoryRows::Messages(rows)
            }
            "deliveries" => {
                let start = if let Some(id) = from {
                    let d = deliveries::get(&tx, id)?.ok_or_else(|| anyhow!("unknown delivery cursor"))?;
                    ensure!(d.room_id == scope.room_id, "delivery cursor belongs to another group");
                    d.room_seq
                } else {
                    0
                };
                let rows=tx.prepare(&format!("SELECT {} FROM deliveries WHERE room_id=?1 AND room_seq>=?2 AND ({UNRESOLVED}) ORDER BY room_seq LIMIT ?3",deliveries::COLS))?.query_map(params![scope.room_id,start,limit],deliveries::row)?.collect::<rusqlite::Result<Vec<_>>>()?;
                HistoryRows::Deliveries(rows)
            }
            _ => anyhow::bail!("kind must be messages, plans or deliveries"),
        };
        tx.commit()?;
        Ok(result)
    }
}

/// 原文的附件、引用和身份独立提供；大型元数据也能单独无损分页。
fn material_metadata(e: &Entry) -> serde_json::Value {
    serde_json::json!({"entryId":e.id,"seq":e.seq,"kind":e.kind,"author":e.data["author"],"attachments":e.data["attachments"],"replyTo":e.data["replyTo"],"mentions":e.data["mentions"],"workspaceFilePaths":e.data["workspaceFilePaths"],"workspaceFileReferences":e.data["workspaceFileReferences"],"deliveryId":e.data["deliveryId"]})
}
