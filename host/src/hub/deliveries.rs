//! 投递事务与事件沿用 Hub 的 `emit_lock`，防止较高 rev 先到导致断线追赶漏行。

use super::Hub;
use crate::LockExt;
use crate::store::{
    Entry,
    deliveries::{Delivery, Status},
};
use anyhow::Result;
use serde_json::{Value, json};

impl Hub {
    /// 提交后的 entry 在同一发布锁内按 rev 发送，接收方失败不撤销已接受请求。
    pub(super) fn delivery_entries(&self, entries: &[Entry]) {
        let mut ordered = entries.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|e| e.rev);
        for entry in ordered {
            let _ = self.events.send(json!({"type":"entry","rev":entry.rev,"entry":entry}));
        }
    }

    /// 用户消息与投递原子入库后一起发布。
    pub fn post_group(&self, room: &str, data: &Value, targets: &[String]) -> Result<Entry> {
        let _guard = self.emit_lock.locked();
        let (entry, cards) = self.store.post_group(room, data, targets)?;
        self.delivery_entries(std::slice::from_ref(&entry));
        self.delivery_entries(&cards);
        crate::analytics::capture(
            self,
            crate::analytics::Event::GroupDeliveryAccepted,
            json!({"kind":"speak","source":"user","targets":targets.len()}).as_object().cloned().unwrap_or_default(),
        );
        Ok(entry)
    }

    /// 工具成功只在事务提交后返回，通知失败由周期调度恢复。
    pub fn enqueue_delivery(&self, d: &Delivery) -> Result<Delivery> {
        let _guard = self.emit_lock.locked();
        let (d, card) = self.store.enqueue_task(d)?;
        self.delivery_entries(&[card]);
        crate::analytics::capture(
            self,
            crate::analytics::Event::GroupDeliveryAccepted,
            json!({"kind":"task","source":"tool","keyed":d.idempotency_key.is_some()})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        Ok(d)
    }

    /// 只条件领取 runner 指定行，并发布 claimed 或预算取消的状态。
    pub fn claim_delivery(&self, id: &str, bot: &str) -> Result<Option<Delivery>> {
        let _guard = self.emit_lock.locked();
        let (d, cards) = self.store.claim_delivery(id, bot)?;
        self.delivery_entries(&cards);
        Ok(d)
    }

    /// 提交 started 并发布卡片后才允许 actor 发送 prompt。
    pub fn start_delivery(&self, id: &str, bot: &str) -> Result<bool> {
        let _guard = self.emit_lock.locked();
        let (started, cards) = self.store.start_delivery(id, bot)?;
        self.delivery_entries(&cards);
        Ok(started)
    }

    /// 保存终态、结果及派生投递，失败由 actor 保留内存结果重试。
    pub fn complete_delivery(
        &self,
        id: &str,
        status: Status,
        text: Option<&str>,
        detail: Option<&str>,
        turn: i64,
    ) -> Result<()> {
        let _guard = self.emit_lock.locked();
        let outcome = serde_json::to_value(status)?;
        let cards = self.store.complete_delivery(id, status, text, detail, turn)?;
        self.delivery_entries(&cards);
        // 等待位超时、runner已经退出的迟到回复仍留在来源群并推送群身份。
        if let Some(entry) = cards.iter().find(|e| e.kind == "agent" && e.data["final"] == true)
            && !self.groups.running(&crate::store::Lane::main(&entry.bot_id))
            && let Err(error) = crate::chat::group::notify(self, &entry.bot_id, entry)
        {
            tracing::warn!(%error,"group notification failed after commit");
        }
        crate::analytics::capture(
            self,
            crate::analytics::Event::GroupDeliveryFinished,
            json!({"outcome":outcome}).as_object().cloned().unwrap_or_default(),
        );
        Ok(())
    }

    /// 持久取消和发布成功后调用方才能停止执行。
    pub fn cancel_deliveries(&self, room: &str, target: Option<&str>, reason: &str) -> Result<()> {
        let _guard = self.emit_lock.locked();
        let (cancelled, cards) = self.store.cancel_deliveries(room, target, reason)?;
        self.delivery_entries(&cards);
        self.stop_cancelled_deliveries(&cancelled);
        Ok(())
    }

    /// 取消通知携带已提交的稳定ID，接收者无需读库，也不会停掉之后的新投递。
    pub(super) fn stop_cancelled_deliveries(&self, deliveries: &[Delivery]) {
        for delivery in deliveries {
            let _ = self.send_cmd(
                &delivery.target_bot_id,
                crate::agent::bot::Cmd::CancelGroup {
                    chat: delivery.room_id.clone(),
                    delivery_ids: vec![delivery.id.clone()],
                },
            );
        }
    }

    /// 新 attempt 与新卡片在同一发布锁中提交。
    pub fn retry_delivery(&self, id: &str) -> Result<Delivery> {
        let _guard = self.emit_lock.locked();
        let (d, card) = self.store.retry_delivery(id)?;
        self.delivery_entries(&[card]);
        Ok(d)
    }
}
