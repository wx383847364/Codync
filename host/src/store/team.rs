//! 两只本机 Bot 的私聊请求及待交付回复。群任务继续由独立的投递表恢复。

use super::entries::{ENTRY_COLS, row_entry, write_entry};
use super::{Entry, EntryKind, Lane, Store, next_rev, now_ms};
use crate::LockExt;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

const REPLY_PREFIX: &str = "team.pendingReply.";

/// 清除被删除聊天的待送正文，必须在删除通知的同一事务内先执行。
pub(super) fn delete_local_replies_for_bot(db: &Connection, bot: &str) -> Result<()> {
    db.execute("DELETE FROM kv WHERE k IN (SELECT ?1 || id FROM entries WHERE bot_id=?2)", params![REPLY_PREFIX, bot])?;
    Ok(())
}

impl Store {
    /// 原子写入双方的入队通知。只有完整落盘的请求才允许重启恢复。
    pub(crate) fn admit_local_request(&self, notices: [(Lane, i64, Value); 2]) -> Result<Vec<Entry>> {
        let mut db = self.db.locked();
        let tx = db.transaction()?;
        let mut entries = Vec::with_capacity(notices.len());
        for (lane, turn, data) in notices {
            entries.push(write_entry(&tx, &uuid::Uuid::new_v4().to_string(), &lane, EntryKind::Notice, turn, &data)?);
        }
        tx.commit()?;
        Ok(entries)
    }

    /// 事务更新双方通知，完成时将完整的待送回复保留在 Host 内部。
    pub(crate) fn update_local_request(
        &self,
        entries: &[(String, Value)],
        reply: Option<(&str, &str)>,
    ) -> Result<Vec<Entry>> {
        let mut db = self.db.locked();
        let tx = db.transaction()?;
        let mut updated = Vec::with_capacity(entries.len());
        for (id, data) in entries {
            let rev = next_rev(&tx)?;
            tx.execute(
                "UPDATE entries SET rev=?2, data=?3, updated_at=?4 WHERE id=?1",
                params![id, rev, data.to_string(), now_ms()],
            )?;
            updated.extend(
                tx.query_row(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id=?1"), [id], row_entry).optional()?,
            );
        }
        if let Some((entry, content)) = reply {
            tx.execute(
                "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![format!("{REPLY_PREFIX}{entry}"), content],
            )?;
        }
        tx.commit()?;
        Ok(updated)
    }

    /// 按接纳顺序读取未结束的本机请求；历史通知和其他来源继续走原过期规则。
    pub(crate) fn pending_local_requests(&self) -> Result<Vec<Entry>> {
        let db = self.db.locked();
        let mut query = db.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE kind='notice'
             AND json_type(data,'$.localBotRequestHops')='integer'
             AND json_extract(data,'$.status') IN ('queued','sent') ORDER BY seq"
        ))?;
        Ok(query.query_map([], row_entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// 按回复落盘顺序恢复待送正文，清理已删除聊天遗留的记录。
    pub(crate) fn pending_local_replies(&self) -> Result<Vec<(String, String, String)>> {
        let db = self.db.locked();
        db.execute(
            "DELETE FROM kv WHERE substr(k,1,length(?1))=?1 AND NOT EXISTS(
                SELECT 1 FROM entries WHERE id=substr(k,length(?1)+1))",
            [REPLY_PREFIX],
        )?;
        let mut query = db.prepare(
            "SELECT entries.id, entries.bot_id, kv.v FROM kv JOIN entries ON kv.k=?1 || entries.id
             WHERE substr(kv.k,1,length(?1))=?1 ORDER BY kv.rowid",
        )?;
        Ok(query
            .query_map([REPLY_PREFIX], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// 回复已交给回合或被用户停止时消费；与回合恢复标记一并提交。
    pub(crate) fn consume_local_replies(&self, entries: &[String], running: Option<(&str, &str, &str)>) -> Result<()> {
        let mut db = self.db.locked();
        let tx = db.transaction()?;
        for entry in entries {
            let consumed = tx.execute("DELETE FROM kv WHERE k=?1", [format!("{REPLY_PREFIX}{entry}")])?;
            ensure!(running.is_none() || consumed == 1, "the pending reply was removed");
        }
        if let Some((bot, key, value)) = running {
            let active: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND deleted=0)", [bot], |row| row.get(0))?;
            ensure!(active, "the reply's bot was deleted");
            tx.execute(
                "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
