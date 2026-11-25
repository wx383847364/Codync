//! 投递事务与事件沿用 Hub 的 `emit_lock`，防止较高 rev 先到导致断线追赶漏行。

use super::Hub;
use crate::LockExt;
use crate::store::{
    Entry,
    deliveries::{Delivery, Status},
};
use anyhow::{Result, ensure};
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

    /// 文件发布与 Stop、退群共用事件锁，只有仍在执行且身份匹配的投递能在来源群保存文件。
    pub fn publish_delivery_file(
        &self,
        id: &str,
        bot: &str,
        lane: &crate::store::Lane,
        turn: i64,
        data: &Value,
    ) -> Result<Entry> {
        let _guard = self.emit_lock.locked();
        ensure!(data["author"].as_str() == Some(bot), "invalid file author");
        let delivery = self.store.delivery(id)?.ok_or_else(|| anyhow::anyhow!("unknown delivery"))?;
        ensure!(
            delivery.status == Status::Started && delivery.target_bot_id == bot && delivery.room_id == lane.chat,
            "group delivery ended"
        );
        let room =
            self.store.bot(&lane.chat)?.filter(|room| !room.deleted).ok_or_else(|| anyhow::anyhow!("unknown group"))?;
        ensure!(
            room.config.is_group() && room.config.members.iter().any(|member| member == bot),
            "bot is no longer in this group"
        );
        // 派工发起者退群同样取消来源关系，不能利用配置保存与取消通知之间的空档发布。
        if let Some(sender) = &delivery.source_bot_id {
            ensure!(
                room.config.members.contains(sender) && self.store.bot(sender)?.is_some_and(|row| !row.deleted),
                "task sender left this group"
            );
        }
        ensure!(self.store.bot(bot)?.is_some_and(|author| !author.deleted), "bot was deleted");
        // 保留发布时核验过的投递身份，使文件本身可以成为回传结果。
        let mut data = data.clone();
        data["deliveryId"] = id.into();
        let entry = self.store.insert_entry(lane, crate::store::EntryKind::Agent, turn, &data)?;
        self.delivery_entries(std::slice::from_ref(&entry));
        Ok(entry)
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
            json!({"kind":"speak","source":"user","targets":entry.data["resolvedMentions"].as_array().map_or(0, Vec::len)})
                .as_object().cloned().unwrap_or_default(),
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

    /// 只条件领取 runner 指定行，并发布真实持久状态；累计次数不取消投递。
    pub fn claim_delivery(&self, id: &str, bot: &str) -> Result<Option<Delivery>> {
        let _guard = self.emit_lock.locked();
        let (d, cards) = self.store.claim_delivery(id, bot)?;
        self.delivery_entries(&cards);
        Ok(d)
    }

    /// 提交 started 并发布任务行后才允许 actor 发送 prompt。
    pub fn start_delivery(&self, id: &str, bot: &str) -> Result<bool> {
        let (started, cards) = {
            let _guard = self.emit_lock.locked();
            let (started, cards) = self.store.start_delivery(id, bot)?;
            self.delivery_entries(&cards);
            (started, cards)
        };
        if started && let Some(card) = cards.first() {
            self.emit_bot(&card.bot_id);
        }
        Ok(started)
    }

    /// 登记引导并同步卡片后，才把用户原文交给当前成员。
    pub fn start_guidance(&self, id: &str, active_id: &str, bot: &str) -> Result<Entry> {
        let _guard = self.emit_lock.locked();
        let (source, cards) = self.store.start_guidance(id, active_id, bot)?;
        self.delivery_entries(&cards);
        Ok(source)
    }

    /// 提交后精准中断当前任务，继续只唤醒原房间的调度器。
    pub(crate) fn set_delivery_paused(
        self: &std::sync::Arc<Self>,
        room: &str,
        id: &str,
        started_at: i64,
        paused: bool,
    ) -> Result<()> {
        let changed = {
            let _guard = self.emit_lock.locked();
            self.store.set_delivery_paused(room, id, started_at, paused).map(|changed| {
                // 在发布锁内先发送中断，再允许同 ID 的继续请求入队。
                if let Some((delivery, card)) = &changed {
                    self.delivery_entries(std::slice::from_ref(card));
                    if paused {
                        self.stop_delivery_turns(std::slice::from_ref(delivery));
                    }
                    if let Some(bot) = self.bot_value(room) {
                        let _ = self.events.send(json!({"type":"bot","rev":bot["rev"],"bot":bot}));
                    }
                }
                changed.is_some()
            })
        };
        let mut properties = json!({"action":if paused {"pause"} else {"resume"},"outcome": match &changed {
            Ok(true) => "applied", Ok(false) => "stale", Err(_) => "error",
        }});
        if let Err(error) = &changed {
            // 只记录粗粒度原因，投递身份和底层错误文案留在本机。
            properties["category"] =
                if error.downcast_ref::<rusqlite::Error>().is_some() { "database" } else { "rejected" }.into();
        }
        crate::analytics::capture(
            self,
            crate::analytics::Event::GroupDeliveryControl,
            properties.as_object().cloned().unwrap_or_default(),
        );
        if changed? && !paused {
            crate::chat::group::ensure_runner(self, room);
        }
        Ok(())
    }

    /// 保存终态、结果及派生投递，失败由 actor 保留内存结果重试。
    pub fn complete_delivery(
        self: &std::sync::Arc<Self>,
        delivery: &Delivery,
        status: Status,
        text: Option<&str>,
        detail: Option<&str>,
        turn: i64,
        guidance: &[(String, crate::store::deliveries::Guidance)],
    ) -> Result<Option<tokio::sync::oneshot::Receiver<()>>> {
        let _guard = self.emit_lock.locked();
        // 群已清空时迟到的执行结果自然结束，让共享成员可以接着处理别的群。
        if self.store.bot(&delivery.room_id)?.is_some_and(|row| row.deleted) {
            self.local_tasks.discard(&delivery.id);
            return Ok(None);
        }
        let id = &delivery.id;
        // 在同一发布锁内接手或提交普通终态，迟到的工具无法插入两者之间。
        if let Some((waiting, entries)) = crate::local_tasks::handoff(self, id, status, turn, guidance)? {
            self.delivery_entries(&entries);
            return Ok(Some(waiting));
        }
        let cards = self.store.complete_delivery(id, status, text, detail, turn, guidance)?;
        if cards.is_empty() {
            return Ok(None);
        }
        self.delivery_entries(&cards);
        // 暂停后的旧回合可能只结算引导状态卡；原派工没有结束，不能重复统计完成。
        if self.store.delivery(id)?.is_some_and(|delivery| delivery.held_after_pause()) {
            return Ok(None);
        }
        self.report_delivery_finished(status, cards.iter().find(|e| e.kind == "agent" && e.data["final"] == true));
        Ok(None)
    }

    /// 后台工作与模型直接交付使用相同的群通知和完成计数。
    pub(crate) fn report_delivery_finished(&self, status: Status, entry: Option<&Entry>) {
        // 等待位超时、runner 已退出的迟到回复仍使用来源群身份。
        if let Some(entry) = entry
            && !self.groups.running(&crate::store::Lane::main(&entry.bot_id))
            && let Err(error) = crate::chat::group::notify(self, &entry.bot_id, entry)
        {
            tracing::warn!(%error,"group notification failed after commit");
        }
        crate::analytics::capture(
            self,
            crate::analytics::Event::GroupDeliveryFinished,
            serde_json::Map::from_iter([("outcome".into(), json!(status))]),
        );
    }

    /// 上下文资源不足只附可恢复说明，不改变 queued；按 rev 顺序发布到来源群。
    pub fn delivery_context_problem(&self, id: &str, bot: &str, detail: &str) -> Result<()> {
        let _guard = self.emit_lock.locked();
        let entries = self.store.delivery_context_problem(id, bot, detail)?;
        self.delivery_entries(&entries);
        Ok(())
    }

    /// 交接和原派工的投影共用事件锁。
    pub(crate) fn group_handoff_update<T>(&self, apply: impl FnOnce() -> Result<(T, Vec<Entry>)>) -> Result<T> {
        let _guard = self.emit_lock.locked();
        let (result, cards) = apply()?;
        self.delivery_entries(&cards);
        Ok(result)
    }

    /// 持久取消和发布成功后调用方才能停止执行。
    pub fn cancel_deliveries(&self, room: &str, target: Option<&str>, reason: &str) -> Result<()> {
        let refresh = {
            let _guard = self.emit_lock.locked();
            let (cancelled, cards) = self.store.cancel_deliveries(room, target, reason)?;
            self.delivery_entries(&cards);
            self.stop_delivery_turns(&cancelled);
            !cancelled.is_empty()
        };
        if refresh {
            self.emit_bot(room);
        }
        Ok(())
    }

    /// 中断通知使用已提交的稳定 ID，接收者只停止匹配的执行或上下文维护。
    pub(super) fn stop_delivery_turns(&self, deliveries: &[Delivery]) {
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
