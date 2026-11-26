//! 用户直接运行登记任务；普通消息仅按完整别名从同一个入口入队。

use super::{ExecutionTask, Job, Outcome, Report, Running, Task, registered, supervise};
use crate::{
    LockExt,
    hub::Hub,
    store::{
        BotConfig, Entry,
        deliveries::{Delivery, DirectTask},
    },
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::oneshot;

/// 目录或派工使用同一范围判定，项目范围从来源群的成员工作目录推导。
pub(super) fn available(hub: &Hub, room: &BotConfig, task: &Task) -> Result<bool> {
    if !task.program.is_absolute()
        || task.title.trim().is_empty()
        || task.title.len() > 160
        || !(1..=3600).contains(&task.timeout_seconds)
    {
        return Ok(false);
    }
    if task.group_ids.contains(&room.id) {
        return Ok(true);
    }
    let Some(project) = &task.project_root else { return Ok(task.group_ids.is_empty()) };
    if !project.is_absolute() {
        return Ok(false);
    }
    let Ok(project) = dunce::canonicalize(project) else { return Ok(false) };
    for member in &room.members {
        if hub.store.bot(member)?.is_some_and(|member| {
            !member.deleted && dunce::canonicalize(&member.config.cwd).is_ok_and(|cwd| cwd.starts_with(&project))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 读取当前房间，排除私聊、删除和关闭的阶段。
pub(super) fn group(hub: &Hub, id: &str) -> Result<BotConfig> {
    hub.store
        .bot(id)?
        .filter(|b| !b.deleted && b.config.is_group() && b.config.stage.as_ref().is_none_or(|s| !s.read_only()))
        .map(|b| b.config)
        .context("This group is unavailable for Host tasks.")
}

/// 固定执行配置的版本随派工保存，公开目录不包含路径或参数。
fn descriptor(id: &str, task: &Task) -> Result<DirectTask> {
    let version = crate::chat::files::digest_hex(&serde_json::to_vec(task)?);
    Ok(DirectTask { task_id: id.into(), title: task.title.clone(), version })
}

/// 为当前群列出可显式执行的本机任务。
pub(crate) async fn catalog(hub: &Hub, room: &str) -> Result<Value> {
    let room = group(hub, room)?;
    let mut tasks = Vec::new();
    for (id, task) in registered().await? {
        if available(hub, &room, &task)? {
            tasks.push(descriptor(&id, &task)?);
        }
    }
    Ok(json!({"items": tasks}))
}

/// 由 Host 写入用户消息和派工；保持正常的群事件发布和调度唤醒。
fn submit(hub: &Arc<Hub>, room: &str, nonce: &str, text: &str, task: &DirectTask) -> Result<(Entry, Entry)> {
    ensure!(!nonce.is_empty() && nonce.len() <= 128, "A Host task requires a valid clientNonce");
    let (entry, card, added) = hub.group_handoff_update(|| {
        ensure!(!hub.local_tasks.stopping.load(Ordering::Acquire), "The Host is stopping");
        let (entry, card, added) = hub.store.post_direct_task(room, nonce, text, task)?;
        let published = if added { vec![entry.clone(), card.clone()] } else { Vec::new() };
        Ok(((entry, card, added), published))
    })?;
    if added {
        crate::analytics::capture(
            hub,
            crate::analytics::Event::GroupDeliveryAccepted,
            serde_json::Map::from_iter([("kind".into(), json!("host_task")), ("source".into(), json!("user"))]),
        );
        if let Err(error) = hub.mark_read(room) {
            tracing::warn!(%error, "could not mark a Host task request read");
        }
        hub.emit_bot(room);
    }
    crate::chat::group::ensure_runner(hub, room);
    Ok((entry, card))
}

/// 显式选择携带目录版本；重复提交可在登记删除后仍找回原卡。
pub(crate) async fn start_direct(hub: &Arc<Hub>, room: &str, nonce: &str, id: &str, version: &str) -> Result<Value> {
    let replay = hub.store.find_by_nonce(room, nonce);
    let task = if let Some(entry) = replay {
        let previous = serde_json::from_value::<DirectTask>(entry.data["directTask"].clone())
            .context("This submission is already bound to another request.")?;
        ensure!(
            previous.task_id == id && previous.version == version,
            "This submission is already bound to another request."
        );
        previous
    } else {
        let room = group(hub, room)?;
        let tasks = registered().await?;
        let task = tasks.get(id).context("This Host task is no longer registered.")?;
        ensure!(available(hub, &room, task)?, "This Host task is unavailable in this group.");
        let result = descriptor(id, task)?;
        ensure!(result.version == version, "This Host task changed. Select it again.");
        result
    };
    let (entry, card) = submit(hub, room, nonce, &task.title, &task)?;
    Ok(json!({"entry":entry,"card":card}))
}

/// 只归一化尾部句号、问候性标点、字母大小写及空格；整句匹配保留否定和引用的含义。
fn command(text: &str) -> String {
    text.trim().trim_end_matches(['。', '！', '!', '.']).split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// 当前用户的简单文字请求唯一匹配时才入队，有歧义时将候选提示给当前输入框。
pub(crate) async fn try_alias(hub: &Arc<Hub>, room: &BotConfig, text: &str, nonce: &str) -> Result<Option<Entry>> {
    if nonce.is_empty() || text.len() > 256 || text.contains(['\r', '\n']) {
        return Ok(None);
    }
    let text_command = command(text);
    if text_command.is_empty() {
        return Ok(None);
    }
    let mut matched = Vec::new();
    // 登记损坏时普通对话仍可发送；目录和显式运行会报告配置错误。
    let Ok(tasks) = registered().await else { return Ok(None) };
    for (id, task) in tasks {
        if task.aliases.iter().any(|alias| command(alias) == text_command) && available(hub, room, &task)? {
            matched.push(descriptor(&id, &task)?);
        }
    }
    match matched.as_slice() {
        [] => Ok(None),
        [task] => Ok(Some(submit(hub, &room.id, nonce, text, task)?.0)),
        multiple => {
            let names = multiple.iter().map(|t| t.title.as_str()).collect::<Vec<_>>().join(", ");
            bail!("Several Host tasks match: {names}. Choose one from the / menu.")
        }
    }
}

/// 将同一份已核验的配置交给进程启动，登记变化只返回原卡阻塞。
pub(super) async fn resolve(hub: &Hub, room: &str, intent: &DirectTask) -> Option<Task> {
    let room = group(hub, room).ok()?;
    let task = registered().await.ok()?.remove(&intent.task_id)?;
    (available(hub, &room, &task).ok()? && descriptor(&intent.task_id, &task).ok().as_ref() == Some(intent))
        .then_some(task)
}

/// 过期或撤销的登记留在原派工，供用户重新选择。
pub(super) fn blocked_report() -> Report {
    Report {
        status: Outcome::Blocked,
        summary: "The Host task registration changed or is no longer available. Select the task again.".into(),
        evidence: vec![],
    }
}

/// 身份独立于 Bot，取得 Host 本机执行位后由相同监督模块处理。
pub(crate) fn dispatch_direct(hub: &Arc<Hub>, delivery: &Delivery) -> Result<Option<oneshot::Receiver<bool>>> {
    if hub.local_tasks.stopping.load(Ordering::Acquire) {
        return Ok(None);
    }
    let Ok(permit) = hub.local_tasks.slot.clone().try_acquire_owned() else { return Ok(None) };
    let intent = hub.store.direct_task(delivery)?;
    let Some(delivery) = hub.group_handoff_update(|| {
        if hub.local_tasks.stopping.load(Ordering::Acquire) {
            return Ok((None, vec![]));
        }
        let (started, entries) = hub.store.start_direct_task(&delivery.id)?;
        if let Some(d) = &started {
            // 注册在发布锁内，关机能等到尚未调度的监督任务。
            hub.local_tasks.running.locked().insert((d.id.clone(), d.started_at), intent.task_id.clone());
        }
        Ok((started, entries))
    })?
    else {
        return Ok(None);
    };
    let (send, answer) = oneshot::channel();
    let hub = hub.clone();
    // 已提交开始边界；进程尚未启动时崩溃也不会造成自动重复执行。
    tokio::spawn(async move {
        let _active = Running { hub: &hub, delivery: &delivery };
        hub.update_keep_awake();
        hub.emit_bot(&delivery.room_id);
        let job =
            Job { id: intent.task_id.clone(), execution: ExecutionTask::Direct(intent), delivery: delivery.clone() };
        supervise(&hub, &job, 0, vec![], Some(permit)).await;
        let _ = send.send(true);
    });
    Ok(Some(answer))
}

#[cfg(test)]
mod tests {
    use super::command;

    /// 仅归一化排版，不丢弃否定词、引用或附加上下文。
    #[test]
    fn exact_alias_keeps_meaningful_words() {
        let registered = command("跑一遍五套软件回归");
        assert_eq!(registered, command(" 跑一遍五套软件回归。 "));
        assert_ne!(registered, command("先别跑一遍五套软件回归"));
        assert_ne!(registered, command("「跑一遍五套软件回归」"));
        assert_ne!(registered, command("跑一遍五套软件回归，然后提交"));
    }
}
