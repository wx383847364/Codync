//! 群投递的持久状态与原子领取；actor 只能领取房间 runner 指定的 ID。

mod completion;
#[cfg(test)]
mod tests;

use super::{Entry, EntryKind, Store, now_ms};
use crate::LockExt;
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Speak,
    Task,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Queued,
    Claimed,
    Started,
    Completed,
    NoReply,
    Failed,
    Interrupted,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub id: String,
    pub room_id: String,
    pub kind: Kind,
    pub source_kind: String,
    pub source_entry_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_bot_id: Option<String>,
    pub target_bot_id: String,
    pub body: Option<String>,
    pub room_seq: i64,
    pub batch_root_id: Option<String>,
    pub attempt: i64,
    pub parent_id: Option<String>,
    pub round: i64,
    pub chain_depth: i64,
    pub idempotency_key: Option<String>,
    pub status: Status,
    pub claimed_at: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub last_error: Option<String>,
}

/// 投递与查询索引同库建立；单调序号通过保留全部投递行计算，不受 UUID 排序影响。
pub(super) fn schema(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS deliveries(
        id TEXT PRIMARY KEY, room_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('speak','task')),
        source_kind TEXT NOT NULL, source_entry_id TEXT, source_execution_id TEXT, source_bot_id TEXT,
        target_bot_id TEXT NOT NULL, body TEXT, room_seq INTEGER NOT NULL, batch_root_id TEXT,
        attempt INTEGER NOT NULL, parent_id TEXT, round INTEGER NOT NULL, chain_depth INTEGER NOT NULL,
        idempotency_key TEXT, status TEXT NOT NULL, claimed_at INTEGER, started_at INTEGER,
        finished_at INTEGER, last_error TEXT);
        CREATE UNIQUE INDEX IF NOT EXISTS delivery_source ON deliveries(source_entry_id,target_bot_id,kind,attempt)
            WHERE source_kind IN ('user','reply');
        CREATE UNIQUE INDEX IF NOT EXISTS delivery_key ON deliveries(source_bot_id,room_id,target_bot_id,idempotency_key)
            WHERE idempotency_key IS NOT NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS delivery_sequence ON deliveries(room_id,room_seq);
        CREATE INDEX IF NOT EXISTS delivery_room ON deliveries(room_id,status,room_seq);
        CREATE INDEX IF NOT EXISTS delivery_batch ON deliveries(batch_root_id,kind,status);
        CREATE INDEX IF NOT EXISTS delivery_pending ON deliveries(status,room_id);")?;
    Ok(())
}

const COLS: &str = "id,room_id,kind,source_kind,source_entry_id,source_execution_id,source_bot_id,target_bot_id,body,room_seq,batch_root_id,attempt,parent_id,round,chain_depth,idempotency_key,status,claimed_at,started_at,finished_at,last_error";

/// 仅接收本模块写出的枚举值；损坏状态通过 SQL 错误显式暴露。
fn row(r: &rusqlite::Row) -> rusqlite::Result<Delivery> {
    let kind: String = r.get(2)?;
    let status: String = r.get(16)?;
    Ok(Delivery {
        id: r.get(0)?,
        room_id: r.get(1)?,
        kind: parse_enum(&kind, 2)?,
        source_kind: r.get(3)?,
        source_entry_id: r.get(4)?,
        source_execution_id: r.get(5)?,
        source_bot_id: r.get(6)?,
        target_bot_id: r.get(7)?,
        body: r.get(8)?,
        room_seq: r.get(9)?,
        batch_root_id: r.get(10)?,
        attempt: r.get(11)?,
        parent_id: r.get(12)?,
        round: r.get(13)?,
        chain_depth: r.get(14)?,
        idempotency_key: r.get(15)?,
        status: parse_enum(&status, 16)?,
        claimed_at: r.get(17)?,
        started_at: r.get(18)?,
        finished_at: r.get(19)?,
        last_error: r.get(20)?,
    })
}

/// 数据库枚举解码保留错误，不把未知状态误当 queued。
fn parse_enum<T: serde::de::DeserializeOwned>(value: &str, index: usize) -> rusqlite::Result<T> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error)))
}

/// 枚举的数据库值和 wire 值共用 serde 命名。
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).expect("enum serialization").as_str().expect("enum string").to_owned()
}

/// 在已有事务中读取投递，不再获取 Store 锁。
fn get(c: &Connection, id: &str) -> Result<Option<Delivery>> {
    Ok(c.query_row(&format!("SELECT {COLS} FROM deliveries WHERE id=?"), [id], row).optional()?)
}

/// 校验房间、目标及发起成员仍有效；入队、领取和发送前都必须调用。
fn eligible(c: &Connection, room: &str, target: &str, source: Option<&str>) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots g JOIN bots t ON t.id=?2
        WHERE g.id=?1 AND g.deleted=0 AND t.deleted=0 AND json_extract(g.config,'$.kind')='group'
        AND EXISTS(SELECT 1 FROM json_each(g.config,'$.members') WHERE value=?2)
        AND (?3 IS NULL OR EXISTS(SELECT 1 FROM json_each(g.config,'$.members') WHERE value=?3)))",
        params![room, target, source],
        |r| r.get(0),
    )?)
}

/// 同一写事务内分配顺序、写入投递，避免跨房间或重试分配冲突。
fn insert(c: &Connection, delivery: &Delivery) -> Result<Delivery> {
    ensure!(
        eligible(c, &delivery.room_id, &delivery.target_bot_id, delivery.source_bot_id.as_deref())?,
        "成员已不在群内"
    );
    let seq: i64 =
        c.query_row("SELECT COALESCE(MAX(room_seq),0)+1 FROM deliveries WHERE room_id=?", [&delivery.room_id], |r| {
            r.get(0)
        })?;
    c.execute(
        "INSERT INTO deliveries(id,room_id,kind,source_kind,source_entry_id,source_execution_id,source_bot_id,
        target_bot_id,body,room_seq,batch_root_id,attempt,parent_id,round,chain_depth,idempotency_key,status)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'queued')",
        params![
            delivery.id,
            delivery.room_id,
            wire(&delivery.kind),
            delivery.source_kind,
            delivery.source_entry_id,
            delivery.source_execution_id,
            delivery.source_bot_id,
            delivery.target_bot_id,
            delivery.body,
            seq,
            delivery.batch_root_id,
            delivery.attempt,
            delivery.parent_id,
            delivery.round,
            delivery.chain_depth,
            delivery.idempotency_key
        ],
    )?;
    let mut inserted = delivery.clone();
    inserted.room_seq = seq;
    Ok(inserted)
}

/// 删除群或成员时在调用方事务内取消投递，并刷新仍存在群的状态卡。
pub(super) fn cancel_for_deletion(c: &Connection, id: &str) -> Result<(Vec<Delivery>, Vec<Entry>)> {
    let changed=c.prepare(&format!("SELECT {COLS} FROM deliveries WHERE (room_id=?1 OR target_bot_id=?1 OR source_bot_id=?1) AND status IN ('queued','claimed','started')"))?
        .query_map([id],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    c.execute("UPDATE deliveries SET status='cancelled',finished_at=?2,last_error='群或成员已删除' WHERE (room_id=?1 OR target_bot_id=?1 OR source_bot_id=?1) AND status IN ('queued','claimed','started')",params![id,now_ms()])?;
    let mut live = Vec::new();
    for delivery in &changed {
        let exists: bool =
            c.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=? AND deleted=0)", [&delivery.room_id], |row| {
                row.get(0)
            })?;
        if exists {
            live.push(delivery.clone());
        }
    }
    Ok((changed, completion::cards(c, live)?))
}

impl Delivery {
    /// 用户消息开启新批次；回复和工具随后从该行继承轮次与链深。
    pub fn speak(room: &str, entry: &str, target: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            room_id: room.into(),
            kind: Kind::Speak,
            source_kind: "user".into(),
            source_entry_id: Some(entry.into()),
            source_execution_id: None,
            source_bot_id: None,
            target_bot_id: target.into(),
            body: None,
            room_seq: 0,
            batch_root_id: Some(entry.into()),
            attempt: 1,
            parent_id: None,
            round: 1,
            chain_depth: 1,
            idempotency_key: None,
            status: Status::Queued,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            last_error: None,
        }
    }
}

/// 在配置更新或独立 Stop 的事务内取消指定成员相关投递，并同步刷新聚合卡。
pub(super) fn cancel_in_transaction(
    c: &Connection,
    room: &str,
    target: Option<&str>,
    reason: &str,
) -> Result<(Vec<Delivery>, Vec<Entry>)> {
    let mut changed = c.prepare(&format!("SELECT {COLS} FROM deliveries WHERE room_id=?1 AND (?2 IS NULL OR target_bot_id=?2 OR source_bot_id=?2) AND status IN ('queued','claimed','started')"))?
        .query_map(params![room,target],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let at = now_ms();
    c.execute("UPDATE deliveries SET status='cancelled',last_error=?3,finished_at=?4 WHERE room_id=?1 AND (?2 IS NULL OR target_bot_id=?2 OR source_bot_id=?2) AND status IN ('queued','claimed','started')",params![room,target,reason,at])?;
    // 返回已提交语义的快照，调用方只在成功提交后通知这些稳定身份。
    for delivery in &mut changed {
        delivery.status = Status::Cancelled;
        delivery.finished_at = Some(at);
        delivery.last_error = Some(reason.to_owned());
    }
    let entries = completion::cards(c, changed.clone())?;
    Ok((changed, entries))
}

impl Store {
    /// 可查询历史状态；数据库错误不会被当成投递不存在。
    pub fn delivery(&self, id: &str) -> Result<Option<Delivery>> {
        get(&self.db.locked(), id)
    }

    /// 房间只按持久入队顺序取候选，actor 不使用此方法自行抢领。
    pub fn queued_deliveries(&self, room: &str) -> Result<Vec<Delivery>> {
        Ok(self
            .db
            .locked()
            .prepare(&format!("SELECT {COLS} FROM deliveries WHERE room_id=? AND status='queued' ORDER BY room_seq"))?
            .query_map([room], row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// 启动、转空闲、周期补查共用的房间查询，目标为空时查全部房间。
    pub fn delivery_rooms(&self, target: Option<&str>) -> Result<Vec<String>> {
        Ok(self
            .db
            .locked()
            .prepare(
                "SELECT DISTINCT room_id FROM deliveries WHERE status='queued' AND (?1 IS NULL OR target_bot_id=?1)",
            )?
            .query_map([target], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// 用户 entry 与全部 speak 在同一事务提交，提交失败不会留下半条指派。
    pub fn post_group(&self, room: &str, data: &Value, targets: &[String]) -> Result<(Entry, Vec<Entry>)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let nonce = data["clientNonce"].as_str().unwrap_or_default();
        if !nonce.is_empty()
            && let Some(entry) = tx
                .query_row(
                    &format!(
                        "SELECT {} FROM entries WHERE bot_id=?1 AND json_extract(data,'$.clientNonce')=?2",
                        super::entries::ENTRY_COLS
                    ),
                    params![room, nonce],
                    super::entries::row_entry,
                )
                .optional()?
        {
            return Ok((entry, vec![]));
        }
        let turn = tx.query_row("SELECT COALESCE(MAX(turn),0)+1 FROM entries WHERE bot_id=?", [room], |r| r.get(0))?;
        let entry = completion::entry(&tx, &uuid::Uuid::new_v4().to_string(), room, EntryKind::User, turn, data)?;
        let mut cards = Vec::new();
        for target in targets {
            insert(&tx, &Delivery::speak(room, &entry.id, target))?;
        }
        if !targets.is_empty() {
            cards.push(completion::card(&tx, &Delivery::speak(room, &entry.id, &targets[0]))?);
        }
        tx.commit()?;
        Ok((entry, cards))
    }

    /// 幂等先于容量，容量与插入共用 IMMEDIATE 事务，重传不会受当前满额影响。
    pub fn enqueue_task(&self, request: &Delivery) -> Result<(Delivery, Entry)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = &request.idempotency_key {
            let original = tx.query_row(&format!("SELECT {COLS} FROM deliveries WHERE source_bot_id=?1 AND room_id=?2 AND target_bot_id=?3 AND idempotency_key=?4"),
                params![request.source_bot_id,request.room_id,request.target_bot_id,key], row).optional()?;
            if let Some(original) = original {
                ensure!(original.body == request.body, "幂等键已用于另一个任务");
                let card = completion::card(&tx, &original)?;
                tx.commit()?;
                return Ok((original, card));
            }
        }
        if let Some(source) = &request.source_execution_id {
            ensure!(
                get(&tx, source)?.is_some_and(|d| d.status == Status::Started
                    && d.room_id == request.room_id
                    && Some(d.target_bot_id) == request.source_bot_id),
                "来源投递已结束"
            );
        }
        let (global, sender): (i64,i64) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(source_bot_id=?1),0) FROM deliveries WHERE kind='task' AND status IN ('queued','claimed','started')",
            [&request.source_bot_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(global < 64 && sender < 16, "群派工容量已满 (全局 64，每成员 16)");
        let delivery = insert(&tx, request)?;
        let card = completion::card(&tx, &delivery)?;
        tx.commit()?;
        Ok((delivery, card))
    }

    /// 仅条件领取 runner 指定的投递；累计发言数不改变已接受任务的生死。
    pub fn claim_delivery(&self, id: &str, bot: &str) -> Result<(Option<Delivery>, Vec<Entry>)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(delivery) = get(&tx, id)? else { return Ok((None, vec![])) };
        if delivery.status != Status::Queued || delivery.target_bot_id != bot {
            return Ok((None, vec![]));
        }
        if !eligible(&tx, &delivery.room_id, bot, delivery.source_bot_id.as_deref())? {
            tx.execute("UPDATE deliveries SET status='cancelled',finished_at=?2,last_error='已不在群内' WHERE id=?1 AND status='queued'", params![id,now_ms()])?;
            let card = completion::card(&tx, &delivery)?;
            tx.commit()?;
            return Ok((None, vec![card]));
        }
        // 同一 ID 只能从 queued 领取一次；成员资格和状态都在本事务内核验。
        let changed = tx.execute(
            "UPDATE deliveries SET status='claimed',claimed_at=?3 WHERE id=?1 AND target_bot_id=?2 AND status='queued'",
            params![id, bot, now_ms()],
        )?;
        let cards = if changed == 1 { vec![completion::card(&tx, &delivery)?] } else { vec![] };
        let result = if changed == 1 { get(&tx, id)? } else { None };
        tx.commit()?;
        Ok((result, cards))
    }

    /// started 提交后才允许发送 prompt；失败保持 claimed，由恢复或后续补查处理。
    pub fn start_delivery(&self, id: &str, bot: &str) -> Result<(bool, Vec<Entry>)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(d) = get(&tx, id)? else { return Ok((false, vec![])) };
        if !eligible(&tx, &d.room_id, bot, d.source_bot_id.as_deref())? {
            bail!("已不在群内");
        }
        // 同一 actor 的受控 fallback 仍属于本次执行；重启 actor 无法领取 started。
        if d.status == Status::Started && d.target_bot_id == bot {
            return Ok((true, vec![]));
        }
        let changed = tx.execute("UPDATE deliveries SET status='started',started_at=?3 WHERE id=?1 AND target_bot_id=?2 AND status='claimed'",params![id,bot,now_ms()])?;
        let card = completion::card(&tx, &d)?;
        tx.commit()?;
        Ok((changed == 1, vec![card]))
    }

    /// Stop 和移除先持久化取消，调用方随后才能停止执行；终态不被覆盖。
    pub fn cancel_deliveries(
        &self,
        room: &str,
        target: Option<&str>,
        reason: &str,
    ) -> Result<(Vec<Delivery>, Vec<Entry>)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (changed, entries) = cancel_in_transaction(&tx, room, target, reason)?;
        tx.commit()?;
        Ok((changed, entries))
    }

    /// 重启恢复保守执行边界：claimed 可重排，started 不自动重放。
    pub fn recover_deliveries(&self) -> Result<()> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed=tx.prepare(&format!("SELECT {COLS} FROM deliveries WHERE status IN ('claimed','started') AND EXISTS(SELECT 1 FROM bots WHERE id=deliveries.room_id AND deleted=0)"))?
            .query_map([],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        tx.execute("UPDATE deliveries SET status='queued',claimed_at=NULL WHERE status='claimed'", [])?;
        tx.execute("UPDATE deliveries SET status='interrupted',finished_at=?1,last_error='host 重启时正在执行，执行结果不确定，请检查后手动重试' WHERE status='started'",[now_ms()])?;
        completion::cards(&tx, changed)?;
        tx.commit()?;
        Ok(())
    }

    /// 主动重试创建新的 attempt 并清除原幂等键，保留所有旧结果。
    pub fn retry_delivery(&self, id: &str) -> Result<(Delivery, Entry)> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut d = get(&tx, id)?.ok_or_else(|| anyhow::anyhow!("unknown delivery"))?;
        ensure!(matches!(d.status, Status::Failed | Status::Interrupted | Status::NoReply), "投递不可重试");
        if let Some(existing) = tx
            .query_row(
                &format!("SELECT {COLS} FROM deliveries WHERE parent_id=?1 ORDER BY attempt DESC LIMIT 1"),
                [id],
                row,
            )
            .optional()?
        {
            let card = completion::card(&tx, &existing)?;
            tx.commit()?;
            return Ok((existing, card));
        }
        if d.kind == Kind::Task {
            let (global,sender):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(source_bot_id=?1),0) FROM deliveries WHERE kind='task' AND status IN ('queued','claimed','started')",[&d.source_bot_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            ensure!(global < 64 && sender < 16, "群派工容量已满 (全局 64，每成员 16)");
        }
        d.parent_id = Some(d.id.clone());
        d.id = uuid::Uuid::new_v4().to_string();
        d.attempt += 1;
        d.idempotency_key = None;
        d.status = Status::Queued;
        let d = insert(&tx, &d)?;
        let card = completion::card(&tx, &d)?;
        tx.commit()?;
        Ok((d, card))
    }
}
