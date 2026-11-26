//! 单个投递的暂停与继续，保留原始任务、顺序和执行身份。

use super::{Delivery, Guidance, Status, Store, completion, eligible, get, wire};
use crate::{
    LockExt,
    store::{Entry, now_ms},
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
    pub executor: Option<String>,
}

impl Delivery {
    /// 暂停状态可能已经被继续请求改为排队，旧执行在成员重新领取前仍需退出。
    pub(crate) fn held_after_pause(&self) -> bool {
        self.status == Status::Paused || (self.started_at.is_some() && self.status == Status::Queued)
    }
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
        anyhow::ensure!(completion::direct_user_mention(&delivery, &source.data), "引导需要直接 @ 这位成员");
        tx.execute(
            "UPDATE deliveries SET status='started',started_at=?2,last_error='引导当前对话' WHERE id=?1",
            params![id, now_ms()],
        )?;
        let cards = vec![completion::card(&tx, &delivery)?];
        tx.commit()?;
        Ok((source, cards))
    }

    /// 后台任务被暂停时结算曾加入的引导；新一轮执行不会重新领用这些 ID。
    pub(crate) fn interrupt_local_task_guidance(&self, guidance: &[(String, Guidance)]) -> Result<Vec<Entry>> {
        if guidance.is_empty() {
            return Ok(vec![]);
        }
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entries = interrupt_guidance(&tx, guidance)?;
        tx.commit()?;
        Ok(entries)
    }

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
            ensure!(super::delivery_eligible(&tx, &delivery)?, "任务的群聊已不可用");
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
                substr(COALESCE(d.body,json_extract(e.data,'$.text'),''),1,160), d.source_kind
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
                    executor: (row.get_ref(5)?.as_str()? == super::DIRECT).then(|| "host".to_owned()),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}

/// 仅更新未被取消的引导；明确未发送的恢复排队，已接受的需人工检查。
pub(super) fn interrupt_guidance(c: &Connection, guidance: &[(String, Guidance)]) -> Result<Vec<Entry>> {
    let mut changed = Vec::new();
    for (guide, outcome) in guidance {
        let queued = matches!(outcome, Guidance::Deferred);
        let next = if queued { Status::Queued } else { Status::Interrupted };
        let detail = if queued {
            "等待下一回合"
        } else {
            "原任务已暂停，引导已中断，请检查进度后重试"
        };
        let count = c.execute(
            "UPDATE deliveries SET status=?2,finished_at=?3,started_at=CASE WHEN ?4 THEN NULL ELSE started_at END,last_error=?5
             WHERE id=?1 AND status='started' AND claimed_at IS NULL",
            params![guide, wire(&next), (!queued).then(now_ms), queued, detail],
        )?;
        if count == 1
            && let Some(delivery) = get(c, guide)?
        {
            changed.push(delivery);
        }
    }
    completion::cards(c, changed)
}
