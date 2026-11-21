//! 单个投递的暂停与继续，保留原始任务、顺序和执行身份。

use super::{Delivery, Status, Store, completion, eligible, get};
use crate::{LockExt, store::Entry};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// 桌面、手机和终端共用的任务行，当前回合时间也是操作校验值。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryControl {
    pub delivery_id: String,
    pub target_bot_id: String,
    pub started_at: i64,
    pub status: String,
    pub preview: String,
}

impl Delivery {
    /// 暂停状态可能已经被继续请求改为排队，旧执行在成员重新领取前仍需退出。
    pub(crate) fn held_after_pause(&self) -> bool {
        self.status == Status::Paused || (self.started_at.is_some() && self.status == Status::Queued)
    }
}

impl Store {
    /// 仅接受当前回合的操作。重复请求、已完成和较旧回合的操作都无需改写。
    pub(crate) fn set_delivery_paused(
        &self,
        room: &str,
        id: &str,
        started_at: i64,
        paused: bool,
    ) -> Result<Option<(Delivery, Entry)>> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut delivery =
            get(&tx, id)?.filter(|d| d.room_id == room).ok_or_else(|| anyhow!("unknown group delivery"))?;
        let expected = if paused { Status::Started } else { Status::Paused };
        // 引导直接加入原回合、未经独立领取，只能随原任务暂停，不能单独重排执行。
        if delivery.status != expected
            || delivery.started_at != Some(started_at)
            || (paused && delivery.claimed_at.is_none())
        {
            return Ok(None);
        }
        if paused {
            tx.execute("UPDATE deliveries SET status='paused' WHERE id=?1", [id])?;
            delivery.status = Status::Paused;
        } else {
            ensure!(!super::super::bots::stage_closed(&tx, room)?, "Restore or reopen the stage to send work.");
            ensure!(eligible(&tx, room, &delivery.target_bot_id, delivery.source_bot_id.as_deref())?, "成员已不在群内");
            // 保留上次的开始时间，调度器用它辨别继续的任务，新回合开始时会更新该时间。
            tx.execute("UPDATE deliveries SET status='queued',claimed_at=NULL,last_error=NULL WHERE id=?1", [id])?;
            delivery.status = Status::Queued;
        }
        let card = completion::card(&tx, &delivery)?;
        // 状态卡和群快照一起提交，断线重连即使只追增量也能恢复操作行。
        super::super::bots::touch_bot(&tx, room)?;
        tx.commit()?;
        Ok(Some((delivery, card)))
    }

    /// 群快照独立于消息分页，包含可控制的回合，以及正在等空闲成员的继续请求。
    pub(crate) fn group_delivery_controls(&self, room: &str) -> Result<Vec<DeliveryControl>> {
        let db = self.db.locked();
        let mut query = db.prepare(
            "SELECT d.id,d.target_bot_id,d.started_at,
                CASE d.status WHEN 'started' THEN 'started' WHEN 'paused' THEN 'paused' ELSE 'resuming' END,
                substr(COALESCE(d.body,json_extract(e.data,'$.text'),''),1,160)
             FROM deliveries d LEFT JOIN entries e ON e.id=d.source_entry_id
             WHERE d.room_id=? AND d.started_at IS NOT NULL AND d.status IN ('queued','claimed','started','paused')
             AND (d.status!='started' OR d.claimed_at IS NOT NULL)
             ORDER BY d.room_seq",
        )?;
        Ok(query
            .query_map(params![room], |row| {
                // 只在任务行保留单行预览，原始内容由任务本身保存。
                let preview = row.get::<_, String>(4)?.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(DeliveryControl {
                    delivery_id: row.get(0)?,
                    target_bot_id: row.get(1)?,
                    started_at: row.get(2)?,
                    status: row.get(3)?,
                    preview,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}
