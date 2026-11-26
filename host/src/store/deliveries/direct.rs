//! 用户直接登记到群派工的 Host 执行意图。

use super::{Delivery, Kind, Status, completion, delivery_eligible, get, insert};
use crate::{
    LockExt,
    store::{Entry, EntryKind, Store, now_ms},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub(crate) const DIRECT: &str = "host_local_task";

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectTask {
    pub task_id: String,
    pub title: String,
    pub version: String,
}

impl Store {
    /// 在一个事务里写入用户可见请求、固定任务版本和原派工卡；重发只取回原记录。
    pub(crate) fn post_direct_task(
        &self,
        room: &str,
        nonce: &str,
        text: &str,
        task: &DirectTask,
    ) -> Result<(Entry, Entry, bool)> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(entry) = tx
            .query_row(
                &format!(
                    "SELECT {} FROM entries WHERE bot_id=?1 AND kind='user' AND json_extract(data,'$.clientNonce')=?2",
                    super::super::entries::ENTRY_COLS
                ),
                params![room, nonce],
                super::super::entries::row_entry,
            )
            .optional()?
        {
            let previous = serde_json::from_value::<DirectTask>(entry.data["directTask"].clone()).ok();
            ensure!(previous.as_ref() == Some(task), "This submission is already bound to another request.");
            let id = format!("delivery-card-{}-1", entry.id);
            let card = tx.query_row(
                &format!("SELECT {} FROM entries WHERE id=?1", super::super::entries::ENTRY_COLS),
                [&id],
                super::super::entries::row_entry,
            )?;
            return Ok((entry, card, false));
        }
        let pending: i64 = tx.query_row(
            "SELECT count(*) FROM deliveries WHERE source_kind=?1 AND status IN ('queued','started','paused')",
            [DIRECT],
            |row| row.get(0),
        )?;
        ensure!(pending < 64, "Too many Host tasks are waiting.");
        let turn =
            tx.query_row("SELECT COALESCE(MAX(turn),0)+1 FROM entries WHERE bot_id=?1", [room], |row| row.get(0))?;
        let id = uuid::Uuid::new_v4().to_string();
        let entry = completion::entry(
            &tx,
            &id,
            room,
            EntryKind::User,
            turn,
            &json!({"text":text,"clientNonce":nonce,"status":"sent","directTask":task}),
        )?;
        let mut delivery = Delivery::speak(room, &entry.id, room);
        delivery.kind = Kind::Task;
        delivery.source_kind = DIRECT.into();
        let delivery = insert(&tx, &delivery)?;
        let card = completion::card(&tx, &delivery)?;
        tx.commit()?;
        Ok((entry, card, true))
    }

    /// 读取不可变的原提交描述；同一任务的手动重试继续使用该登记版本。
    pub(crate) fn direct_task(&self, delivery: &Delivery) -> Result<DirectTask> {
        let source =
            delivery.source_entry_id.as_deref().and_then(|id| self.entry(id)).context("Missing Host task request")?;
        Ok(serde_json::from_value(source.data["directTask"].clone())?)
    }

    /// 只有拿到本机执行位才跨过持久启动边界，未启动任务重启后仍排队。
    pub(crate) fn start_direct_task(&self, id: &str) -> Result<(Option<Delivery>, Vec<Entry>)> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(delivery) = get(&tx, id)?.filter(|d| d.is_host_task() && d.status == Status::Queued) else {
            return Ok((None, vec![]));
        };
        ensure!(delivery_eligible(&tx, &delivery)?, "The group is no longer available.");
        ensure!(
            !super::super::bots::stage_closed(&tx, &delivery.room_id)?,
            "Restore or reopen the stage to send work."
        );
        tx.execute("UPDATE deliveries SET status='started',claimed_at=?2,started_at=MAX(?2,COALESCE(started_at+1,?2)),last_error=NULL WHERE id=?1", params![id, now_ms()])?;
        let result = get(&tx, id)?;
        let card = completion::card(&tx, &delivery)?;
        tx.commit()?;
        Ok((result, vec![card]))
    }
}
