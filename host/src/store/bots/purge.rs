//! 群永久删除的事务、源群内容清理和磁盘恢复回执。

use super::super::{Entry, EntryKind, Lane, Store};
use super::{delete_bot_on, delete_bot_resources_on};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{Connection, params};

pub(crate) struct Deletion {
    pub rev: i64,
    pub cancelled: Vec<crate::store::deliveries::Delivery>,
    pub cards: Vec<Entry>,
}

/// 清除群派工、会话归档和各种推进状态，只按精确的群来源或 Bot 归属匹配。
fn scrub_purged_chat_on(c: &Connection, id: &str) -> Result<()> {
    // 旧版私聊提示是原始文字，只在最近一次共享会话归档来自本群时删除。
    c.execute(
        "DELETE FROM kv AS pending WHERE k GLOB 'group.handoff.private.*'
         AND (CASE WHEN json_valid(v) THEN json_type(v,'$.room') IS NULL ELSE 1 END)
         AND (SELECT json_extract(archive.v,'$.roomId') FROM kv AS archive
              WHERE substr(archive.k,1,length('session.archive.' || substr(pending.k,length('group.handoff.private.')+1) || '.')) =
                    'session.archive.' || substr(pending.k,length('group.handoff.private.')+1) || '.'
                AND json_extract(archive.v,'$.slot') IS NULL
              ORDER BY json_extract(archive.v,'$.archivedAt') DESC LIMIT 1)=?1",
        [id],
    )?;
    c.execute(
        "DELETE FROM kv WHERE k IN (SELECT 'group.handoff.' || id FROM deliveries WHERE room_id=?1)
         OR k IN ('group.handoff.private.' || ?1, 'turn.inflight.' || ?1, 'handoff.' || ?1,
                  'naming.turns.' || ?1, 'fast-mode.' || ?1)
         OR k GLOB ?2 OR k GLOB ?3 OR k GLOB ?4 OR k GLOB ?5 OR k GLOB ?6 OR k GLOB ?7
         OR CASE WHEN json_valid(v) THEN
             (k GLOB 'session.archive.*' AND json_extract(v,'$.roomId')=?1)
             OR (k GLOB 'group.member-session.*' AND json_extract(v,'$.room')=?1)
             OR (k GLOB 'group.handoff.private.*' AND json_extract(v,'$.room')=?1)
             OR (k GLOB 'call.*' AND json_extract(v,'$.bot')=?1)
            ELSE 0 END",
        params![
            id,
            format!("compaction.summary.{id}.*"),
            format!("session.archive.{id}.*"),
            format!("group.context.compact.{id}.*"),
            format!("session.scoped-usage.*.group/{id}/*"),
            format!("session.scoped-usage.{id}.*"),
            format!("group.member-session.{id}.*")
        ],
    )?;
    c.execute("DELETE FROM deliveries WHERE room_id=?1", [id])?;
    c.execute("DELETE FROM turn_usage_attempts WHERE room_id=?1 OR (bot_id=?1 AND room_id IS NULL)", [id])?;
    Ok(())
}

/// 向所有客户端同步撤下记录；版本和 ID 仅用于覆盖曾下载过的那张卡。
fn withdraw_group_cards(c: &Connection, room: &str) -> Result<Vec<Entry>> {
    let cards = c
        .prepare(
            "SELECT id,bot_id,turn FROM entries WHERE kind='notice' AND
         (json_extract(data,'$.memberSessionRoom')=?1 OR json_extract(data,'$.groupDeliveryId') IN
            (SELECT id FROM deliveries WHERE room_id=?1))",
        )?
        .query_map([room], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    cards
        .into_iter()
        .map(|(id, bot, turn)| {
            // 移除正文和来源，只保留客户端已支持的隐藏标记。
            super::super::entries::write_entry(
                c,
                &id,
                &Lane::main(&bot),
                EntryKind::Notice,
                turn,
                &serde_json::json!({"text":"","removed":true}),
            )
        })
        .collect()
}

impl Store {
    /// 将群和所有待删成员一起提交，并留下一条可重试磁盘清理的记录。
    pub(crate) fn purge_group(&self, ids: &[String], cleanup_key: &str) -> Result<Vec<Deletion>> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut deleted = Vec::with_capacity(ids.len());
        for id in ids {
            let (rev, cancelled, mut cards) = delete_bot_on(&tx, id)?;
            // 共享 Bot 的私聊保留，撤下只属于被删群的系统交接摘要。
            let withdrawn = withdraw_group_cards(&tx, id)?;
            cards.retain(|card| !withdrawn.iter().any(|entry| entry.id == card.id));
            cards.extend(withdrawn);
            scrub_purged_chat_on(&tx, id)?;
            // 保留同步所需的 ID 和群编号用的名称，去掉角色配置、路径、目标与交接内容。
            tx.execute(
                "UPDATE bots SET config=json_object('id', id, 'name', json_extract(config, '$.name'),
                        'kind', COALESCE(json_extract(config, '$.kind'), 'agent')) WHERE id=?1",
                [id],
            )?;
            deleted.push(Deletion { rev, cancelled, cards });
        }
        tx.execute("INSERT INTO kv(k,v) VALUES(?1,?2)", params![cleanup_key, serde_json::to_string(ids)?])?;
        tx.commit()?;
        Ok(deleted)
    }

    /// 停止执行者后清扫迟到的私有状态，只有完整清理后才移除恢复记录。
    pub(crate) fn finish_group_purge(&self, ids: &[String], key: &str) -> Result<()> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for id in ids {
            scrub_purged_chat_on(&tx, id)?;
            delete_bot_resources_on(&tx, id)?;
        }
        tx.execute("DELETE FROM kv WHERE k=?1", [key])?;
        tx.commit()?;
        Ok(())
    }
}
