//! 群聊的唯一房间调度器。投递表决定谁执行，忙成员保持 queued。
//! speak/task 共用十分钟等待位；等待超时不终止执行，已接受请求不被新消息覆盖。
//! 持续执行有效派工，交付或需要用户输入时自然等待；累计轮次与链深不终止协作。

use crate::LockExt;
use crate::agent::bot::Cmd;
use crate::hub::{BotStatus, Hub};

use crate::store::{BotConfig, BotKind, Entry, EntryKind, Lane, Store};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

mod context;
pub(crate) mod history;
pub use context::delivery_prompt;

/// 补充背景预算供无正文埋点使用，不包含受保护任务原文。
pub(crate) fn background_budget() -> usize {
    context::BACKGROUND_TOKENS
}

/// 十分钟只释放房间等待位，不把在途投递视为 pass 或失败。
const MEMBER_TIMEOUT: Duration = Duration::from_mins(10);

const PASS: &str = "(pass)";

/// 每房间一个 runner；值只表示队列有更新，不决定投递生死。
pub struct Rooms {
    /// 已注册 runner 的 dirty 标记。
    running: Mutex<HashMap<String, bool>>,
    /// How long the room waits on a member: [`MEMBER_TIMEOUT`] in milliseconds.
    member_timeout: AtomicU64,
}

impl Default for Rooms {
    /// 初始化每房间唯一 runner 注册表和十分钟等待上限。
    fn default() -> Self {
        Self {
            running: Mutex::default(),
            member_timeout: AtomicU64::new(MEMBER_TIMEOUT.as_millis().try_into().unwrap_or(u64::MAX)),
        }
    }
}

impl Rooms {
    fn member_timeout(&self) -> Duration {
        Duration::from_millis(self.member_timeout.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub fn set_member_timeout(&self, timeout: Duration) {
        self.member_timeout.store(timeout.as_millis().try_into().unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// A room turn is running in `lane`'s group.
    pub fn running(&self, lane: &Lane) -> bool {
        self.running.locked().contains_key(&lane.chat)
    }
}

/// Existing, distinct agent bots (not groups, not the group itself).
pub fn valid_members(store: &Store, group_id: &str, ids: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for id in ids.iter().map(|i| i.trim()) {
        if id.is_empty() || id == group_id || out.iter().any(|o| o == id) {
            continue;
        }
        match store.bot(id)? {
            Some(row) if !row.deleted && row.config.kind == BotKind::Agent => out.push(id.to_owned()),
            Some(row) if row.config.is_group() => bail!("a group can't be a member of another group"),
            _ => {}
        }
    }
    if out.is_empty() {
        bail!("a group needs at least one bot");
    }
    Ok(out)
}

/// A group whose members are exactly `members`.
pub fn with_members(store: &Store, members: &[String]) -> Result<Option<String>> {
    let mut wanted = members.to_vec();
    wanted.sort();
    Ok(store.bots()?.into_iter().find_map(|row| {
        let mut have = row.config.members.clone();
        have.sort();
        (!row.deleted && row.config.is_group() && have == wanted).then_some(row.config.id)
    }))
}

/// 持久取消该群当前已接受的投递，再由 Hub 按投递 ID 停止对应执行。
pub fn stop(hub: &Hub, group: &str) -> Result<()> {
    hub.cancel_deliveries(group, None, "群聊已停止")
}

/// 消息入库后唤醒唯一房间 runner；新消息不取消已接受投递。
pub fn start(hub: &Arc<Hub>, group: &BotConfig, _lane: Lane) {
    ensure_runner(hub, &group.id);
}

/// 幂等唤醒：已有 runner 时只置 dirty，避免退出窗口漏掉提交后通知。
pub fn ensure_runner(hub: &Arc<Hub>, room: &str) {
    let mut running = hub.groups.running.locked();
    if let Some(dirty) = running.get_mut(room) {
        *dirty = true;
        return;
    }
    running.insert(room.to_owned(), false);
    let (hub, room) = (hub.clone(), room.to_owned());
    tokio::spawn(async move {
        if let Err(error) = run(&hub, &room).await {
            tracing::error!(%room, %error, "delivery runner paused");
            hub.groups.running.locked().remove(&room);
            hub.emit_bot(&room);
        }
    });
}

/// 四种唤醒统一指向房间；查询或通知失败保留 queued，交给周期补查。
pub fn wake(hub: &Arc<Hub>, target: Option<&str>) {
    match hub.store.delivery_rooms(target) {
        Ok(rooms) => {
            for room in rooms {
                ensure_runner(hub, &room);
            }
        }
        Err(error) => tracing::error!(%error,"delivery wake query failed"),
    }
}

/// 单 runner 按 `room_seq` 推进，忙成员本轮跳过；task 与 speak 都占房间等待位。
async fn run(hub: &Arc<Hub>, room: &str) -> Result<()> {
    let lane = Lane::main(room);
    let visible_before = hub.store.messages_after(&lane, 0, 1)?.last().map_or(0, |entry| entry.seq);
    let mut deferred = std::collections::HashSet::new();
    loop {
        let candidates = hub.store.queued_deliveries(room)?;
        let next = candidates.into_iter().find(|d| !deferred.contains(&d.id));
        let Some(delivery) = next else {
            let mut running = hub.groups.running.locked();
            if running.get(room) == Some(&true) {
                running.insert(room.to_owned(), false);
                deferred.clear();
                continue;
            }
            // 在同一锁内移除，随后到达的通知会建立新的 runner。
            running.remove(room);
            drop(running);
            // 成员最后的 idle 事件可能先于 runner 退出，必须再发布一次真实房间状态。
            hub.emit_bot(room);
            // 退出后的通知是尽力操作，失败不能让旧runner清掉后来者的注册标记。
            match hub.store.messages_after(&lane, 0, 1) {
                Ok(entries) => {
                    if let Some(entry) =
                        entries.last().filter(|e| e.seq > visible_before && e.kind == EntryKind::Agent.as_str())
                        && let Err(error) = notify(hub, room, entry)
                    {
                        tracing::warn!(%error,"group notification failed");
                    }
                }
                Err(error) => tracing::warn!(%error,"group notification lookup failed after runner exit"),
            }
            return Ok(());
        };
        let valid = hub
            .store
            .bot(room)?
            .filter(|r| !r.deleted)
            .is_some_and(|r| r.config.members.contains(&delivery.target_bot_id));
        if !valid {
            hub.cancel_deliveries(room, Some(&delivery.target_bot_id), "已不在群内")?;
            continue;
        }
        let runtime = hub.runtime(&delivery.target_bot_id);
        if matches!(runtime.status, BotStatus::Working | BotStatus::NeedsInput) || runtime.storage_error.is_some() {
            deferred.insert(delivery.id);
            continue;
        }
        let (reply, answer) = oneshot::channel();
        if hub.send_cmd(&delivery.target_bot_id, Cmd::Delivery { id: delivery.id.clone(), reply }).is_err() {
            deferred.insert(delivery.id);
            continue;
        }
        // 信号仅结束等待；超时和接收端关闭都不决定持久投递的生死。
        if let Ok(Ok(false) | Err(_)) = tokio::time::timeout(hub.groups.member_timeout(), answer).await {
            deferred.insert(delivery.id);
        }
    }
}

/// 群回复使用群通知身份，绝不向成员私聊发送 Finished。
pub fn notify(hub: &Hub, room: &str, entry: &Entry) -> Result<()> {
    let Some(group) = hub.store.bot(room)?.filter(|r| !r.deleted) else { return Ok(()) };
    let from = entry.data["author"].as_str().unwrap_or_default();
    let name = hub.bot_name(from);
    crate::remote::push::notify(
        hub,
        &group.config,
        Some(from),
        &format!("{name} in {}", group.config.name),
        entry.data["text"].as_str().unwrap_or_default(),
        crate::remote::push::AlertKind::Done,
    );
    Ok(())
}

/// A member's final text is a pass (the room doesn't see it).
pub fn is_pass(text: &str) -> bool {
    let t = text.trim().trim_matches(|c: char| c == '"' || c == '.' || c == '*').to_lowercase();
    t.is_empty() || t == PASS || t == "pass"
}

#[cfg(test)]
mod tests;
