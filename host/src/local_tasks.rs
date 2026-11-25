//! 用户登记的本地任务：成员交出原投递，Host 等待终态并发布来源群证据。

use crate::LockExt;
use crate::agent::process::{self, Process};
use crate::chat::files::{self, Snapshot};
use crate::hub::Hub;
use crate::store::deliveries::{Delivery, Guidance, Status};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

pub(crate) const START: &str = "start_local_task";
pub(crate) const LIST: &str = "list_local_tasks";
const POLL: Duration = Duration::from_millis(250);
const QUEUE_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Task {
    title: String,
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    timeout_seconds: u64,
}

struct Job {
    id: String,
    task: Task,
    delivery: Delivery,
}

type Execution = (String, Option<i64>);

/// 登记只保留到当前 ACP 回合结束；真正执行时原投递仍持久为 started。
pub(crate) struct LocalTasks {
    reserved: Mutex<HashMap<String, Job>>,
    running: Mutex<HashMap<Execution, String>>,
    stopping: AtomicBool,
    slot: Semaphore,
}

impl Default for LocalTasks {
    /// 同一 Host 的本地任务共用一个执行位，桌面验收不会互相抢占。
    fn default() -> Self {
        Self {
            reserved: Mutex::default(),
            running: Mutex::default(),
            stopping: AtomicBool::new(false),
            slot: Semaphore::new(1),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    task_id: String,
    context_delivery_id: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Passed,
    Failed,
    Blocked,
    NeedsReview,
}

impl Outcome {
    /// 需要复核是已交付的结果，但只能由人工确认，不能触发自动回传链。
    fn label(self) -> &'static str {
        match self {
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Blocked => "Blocked",
            Self::NeedsReview => "Needs review",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    status: Outcome,
    summary: String,
    #[serde(default)]
    evidence: Vec<String>,
}

/// 为现有 chat 工具桥增加受控登记入口，调用者不能提交命令或目标房间。
pub(crate) fn tools() -> [Value; 2] {
    [
        json!({"name":LIST,"description":"List tasks registered by the user on this Host. Available during a group assignment.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
            "annotations":{"readOnlyHint":true}}),
        json!({"name":START,"description":"Hand the current group assignment to one registered local task. Finish your turn after it is accepted; the Host runs it and updates the original assignment when it ends. Do not wait or poll.",
            "inputSchema":{"type":"object","properties":{"taskId":{"type":"string"},"contextDeliveryId":{"type":"string"}},
                "required":["taskId","contextDeliveryId"],"additionalProperties":false},
            "annotations":{"readOnlyHint":false,"idempotentHint":true}}),
    ]
}

/// 本机管理者登记的程序使用明文参数数组执行；模型只可选择任务 ID。
async fn registered() -> Result<BTreeMap<String, Task>> {
    let path = crate::service::data_dir().join("local-tasks.json");
    let data = match tokio::fs::read(&path).await {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error).context("reading local task registry"),
    };
    serde_json::from_slice(&data).context("invalid local task registry")
}

/// 从可信工具桥取执行身份，普通群也用派工 ID 防止旧请求落到新回合。
pub(crate) async fn call(hub: &Hub, bot: &str, group: Option<&str>, name: &str, args: &Value) -> Result<Value> {
    ensure!(!hub.local_tasks.stopping.load(Ordering::Acquire), "the Host is stopping");
    let (_, id) = hub.team.scoped_delivery(bot, group)?.context("local tasks require an active group assignment")?;
    let tasks = registered().await?;
    if name == LIST {
        return Ok(
            json!({"items":tasks.iter().map(|(id, task)|json!({"taskId":id,"title":task.title})).collect::<Vec<_>>()}),
        );
    }
    let request = serde_json::from_value::<Request>(args.clone())?;
    ensure!(request.context_delivery_id == id, "contextDeliveryId does not match the current group execution");
    let task = tasks.get(&request.task_id).context("unknown registered local task")?;
    ensure!(
        task.program.is_absolute()
            && !task.title.trim().is_empty()
            && task.title.len() <= 160
            && (1..=3600).contains(&task.timeout_seconds),
        "invalid registered local task"
    );
    let cfg = hub.store.bot(bot)?.filter(|bot| !bot.deleted).context("unknown bot")?.config;
    let delivery = hub
        .store
        .delivery(&id)?
        .filter(|d| d.status == Status::Started && d.target_bot_id == bot)
        .context("group assignment ended")?;
    let accepted = json!({"taskId":request.task_id,"runId":id,"status":"accepted"});
    {
        if let Some(job) = hub.local_tasks.reserved.locked().get(&id) {
            ensure!(
                job.id == request.task_id && job.delivery.started_at == delivery.started_at,
                "a different local task is already registered for this assignment"
            );
            return Ok(accepted);
        }
    }
    crate::chat::manage::approve(hub, &cfg, group, "Run local task".into(), task.title.clone()).await?;
    // 审批期间可能发生停止、继续或切群；入队与 actor 收尾共用发布锁。
    hub.group_handoff_update(|| {
        ensure!(
            hub.team.scoped_delivery(bot, group)?.is_some_and(|(_, active)| active == id) && live(hub, &delivery)?,
            "group assignment ended"
        );
        if let Some(task_id) = hub.local_tasks.running.locked().get(&(id.clone(), delivery.started_at)) {
            ensure!(*task_id == request.task_id, "a different local task already owns this assignment");
            return Ok((accepted, Vec::new()));
        }
        let mut reserved = hub.local_tasks.reserved.locked();
        if let Some(job) = reserved.get(&id) {
            ensure!(job.id == request.task_id, "a different local task is already registered for this assignment");
        } else {
            reserved.insert(id, Job { id: request.task_id, task: task.clone(), delivery });
        }
        Ok((accepted, Vec::new()))
    })
}

/// 在调用方发布锁内结束登记并释放模型；后台任务完成前继续保留原投递。
pub(crate) fn handoff(
    hub: &Arc<Hub>,
    id: &str,
    status: Status,
    turn: i64,
    guidance: &[(String, Guidance)],
) -> Result<Option<(oneshot::Receiver<()>, Vec<crate::store::Entry>)>> {
    let mut reserved = hub.local_tasks.reserved.locked();
    let Some(job) = reserved.get(id) else { return Ok(None) };
    if !matches!(status, Status::Completed | Status::NoReply) || !live(hub, &job.delivery)? {
        reserved.remove(id);
        return Ok(None);
    }
    let detail = format!("{} · Queued", job.task.title);
    let entries = hub.store.local_task_activity(id, job.delivery.started_at, &detail)?;
    if let Some(job) = reserved.remove(id)
        && !entries.is_empty()
    {
        hub.local_tasks.running.locked().insert((id.to_owned(), job.delivery.started_at), job.id.clone());
        let (hub, guidance) = (hub.clone(), guidance.to_vec());
        let (finished, waiting) = oneshot::channel();
        // 保持本批次顺序直到后台任务结束，模型 actor 本身可以释放。
        tokio::spawn(async move {
            hub.update_keep_awake();
            let _active = Running { hub: &hub, delivery: &job.delivery };
            supervise(&hub, &job, turn, guidance).await;
            let _ = finished.send(());
        });
        return Ok(Some((waiting, entries)));
    }
    Ok(None)
}

/// 旧回合退出时释放自己的执行登记，写回前核对开始时间。
struct Running<'a> {
    hub: &'a Hub,
    delivery: &'a Delivery,
}

impl Drop for Running<'_> {
    /// 新回合可能已经接手相同投递；旧后台退出不能删新登记。
    fn drop(&mut self) {
        self.hub.local_tasks.running.locked().remove(&(self.delivery.id.clone(), self.delivery.started_at));
        self.hub.update_keep_awake();
    }
}

impl LocalTasks {
    /// 先阻止新登记，再让监督者结束所有属于本 Host 的进程组。
    pub(crate) fn stop_admission(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// 等待已登记的各代执行回收进程，其中包括正在停止的旧回合。
    pub(crate) async fn shutdown(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while self.busy() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
        }
    }

    /// 已删除来源群的 actor 收尾仍要撤掉登记。
    pub(crate) fn discard(&self, id: &str) {
        self.reserved.locked().remove(id);
    }

    /// 防止后台执行排队或运行期间 Host 被当成空闲。
    pub(crate) fn busy(&self) -> bool {
        !self.running.locked().is_empty()
    }
}

/// 停止、退群、永久删除及暂停继续均由既有投递状态决定；旧工作不能覆盖新回合。
fn live(hub: &Hub, delivery: &Delivery) -> Result<bool> {
    Ok(!hub.local_tasks.stopping.load(Ordering::Acquire)
        && hub
            .store
            .delivery(&delivery.id)?
            .is_some_and(|current| current.status == Status::Started && current.started_at == delivery.started_at))
}

/// 持有执行位贯穿程序运行与清理，结果发布失败时保留工作目录供本机核对。
async fn supervise(hub: &Arc<Hub>, job: &Job, turn: i64, guidance: Vec<(String, Guidance)>) {
    let run = run(hub, job).await;
    if matches!(run, Ok(None)) || !live(hub, &job.delivery).unwrap_or(true) {
        reconcile_guidance(hub, &job.delivery.room_id, &guidance);
        return;
    }
    let (report, snapshots) = match run {
        Ok(Some(result)) => result,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(delivery=%job.delivery.id,%error,"registered local task failed");
            (
                Report {
                    status: Outcome::Failed,
                    summary: "The local task did not produce a valid result.".into(),
                    evidence: vec![],
                },
                vec![],
            )
        }
    };
    let files: Vec<_> = snapshots.iter().map(|file| file.meta.clone()).collect();
    let mut snapshots = snapshots;
    let status = match report.status {
        Outcome::Passed | Outcome::NeedsReview => Status::Completed,
        Outcome::Failed | Outcome::Blocked => Status::Failed,
    };
    let detail = format!("{} · {}", job.task.title, report.status.label());
    let mut text = format!("{detail}\n\n{}", report.summary);
    if !files.is_empty() {
        let names = files.iter().map(|file| file.name.as_str()).collect::<Vec<_>>().join(", ");
        let _ = write!(text, "\n\nEvidence: {names}");
    }
    let finish = crate::store::deliveries::LocalTaskResult {
        started_at: job.delivery.started_at,
        status,
        detail: &detail,
        text: &text,
        files: &files,
        follow_up: matches!(report.status, Outcome::Passed),
        turn,
        guidance: &guidance,
    };
    let mut delay = Duration::from_secs(1);
    while !hub.local_tasks.stopping.load(Ordering::Acquire) {
        match hub.group_handoff_update(|| {
            let entries = hub.store.complete_local_task(&job.delivery.id, &finish)?;
            let report = entries.iter().find(|entry| entry.data["localTask"] == true).cloned();
            if report.is_some() {
                for snapshot in &mut snapshots {
                    snapshot.commit();
                }
            }
            Ok((report, entries))
        }) {
            Ok(published) => {
                if let Some(entry) = &published {
                    hub.emit_bot(&job.delivery.room_id);
                    hub.report_delivery_finished(status, Some(entry));
                    crate::chat::group::ensure_runner(hub, &job.delivery.room_id);
                    metric(hub, report.status.label(), (status == Status::Failed).then_some("execution"));
                } else {
                    metric(hub, "interrupted", Some("scope"));
                }
                return;
            }
            Err(error) => {
                if delay == Duration::from_secs(1) {
                    tracing::warn!(delivery=%job.delivery.id,%error,"could not save local task result");
                    metric(hub, "save_failed", Some("storage"));
                }
                // 停止或 Host 关闭时无需等完整退避间隔。
                let retry = tokio::time::Instant::now() + delay;
                while tokio::time::Instant::now() < retry {
                    if !live(hub, &job.delivery).unwrap_or(true) {
                        reconcile_guidance(hub, &job.delivery.room_id, &guidance);
                        return;
                    }
                    tokio::time::sleep(POLL).await;
                }
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// 原执行被取消时清理已加入的引导并唤醒原群。
fn reconcile_guidance(hub: &Arc<Hub>, room: &str, guidance: &[(String, Guidance)]) {
    if let Err(error) = hub.group_handoff_update(|| Ok(((), hub.store.interrupt_local_task_guidance(guidance)?))) {
        tracing::warn!(%error, "could not reconcile guidance after stopping a local task");
    }
    crate::chat::group::ensure_runner(hub, room);
}

/// 等待本机执行位期间也查取消；按固定任务配置启动独立进程组并等实际终码。
async fn run(hub: &Arc<Hub>, job: &Job) -> Result<Option<(Report, Vec<Snapshot>)>> {
    let wait = tokio::time::Instant::now() + QUEUE_TIMEOUT;
    let permit = loop {
        if !live(hub, &job.delivery)? {
            return Ok(None);
        }
        if let Ok(permit) = hub.local_tasks.slot.try_acquire() {
            break permit;
        }
        if tokio::time::Instant::now() >= wait {
            return Ok(Some((
                Report {
                    status: Outcome::Blocked,
                    summary: "The local task waited too long for the Host.".into(),
                    evidence: vec![],
                },
                vec![],
            )));
        }
        tokio::time::sleep(POLL).await;
    };
    let _permit = permit;
    let group_dir = files::root(&job.delivery.room_id);
    let dir = group_dir
        .parent()
        .context("missing group directory")?
        .join("local-task-runs")
        .join(&job.delivery.id)
        .join(job.delivery.started_at.context("missing task start")?.to_string());

    let detail = format!("{} · Running", job.task.title);
    let mut command = tokio::process::Command::new(&job.task.program);
    let result_path = dir.join("result.json");
    command.args(&job.task.args).current_dir(&dir).env("CODYNC_TASK_RESULT", &result_path).stdin(Stdio::null());
    process::own_tree(&mut command);
    // 在同一个发布锁内复核来源后启动，停止请求不会插入校验和 spawn 之间。
    let child = hub.group_handoff_update(|| {
        if !live(hub, &job.delivery)? {
            return Ok((None, vec![]));
        }
        let entries = hub.store.local_task_activity(&job.delivery.id, job.delivery.started_at, &detail)?;
        let child = (!entries.is_empty()).then(|| {
            std::fs::create_dir_all(dir.parent().context("missing run parent")?)?;
            std::fs::create_dir(&dir).context("creating unique task run folder")?;
            command.stdout(std::fs::File::create(dir.join("stdout.log"))?);
            command.stderr(std::fs::File::create(dir.join("stderr.log"))?);
            Process::new(command.spawn()?)
        });
        Ok((child, entries))
    })?;
    let Some(mut child) = child.transpose()? else { return Ok(None) };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(job.task.timeout_seconds);
    let exit = loop {
        if !live(hub, &job.delivery)? {
            child.kill().await;
            return Ok(None);
        }
        if child.exited()? {
            // 终止仍被拥有的进程组后再读缓存终码，避免回收后向重用的 PID 发信号。
            child.kill().await;
            break child.exit_code();
        }
        if tokio::time::Instant::now() >= deadline {
            child.kill().await;
            return Ok(Some((
                Report {
                    status: Outcome::Blocked,
                    summary: "The local task exceeded its time limit.".into(),
                    evidence: vec![],
                },
                vec![],
            )));
        }
        tokio::time::sleep(POLL).await;
    };
    let evidence_root = files::root(&job.delivery.room_id);
    // 证据目录只能在来源群仍存在时建立；后台复制不会复活已删除的目录。
    let hub = Arc::clone(hub);
    let room = job.delivery.room_id.clone();
    Ok(Some(
        tokio::task::spawn_blocking(move || {
            hub.with_live_bot(&room, |_| Ok(std::fs::create_dir_all(&evidence_root)?))?;
            read_report(&dir, &result_path, &evidence_root, exit)
        })
        .await??,
    ))
}

/// 只复制本轮目录内的常规文件；不把本机绝对路径、任意 stdout 或链接发到群。
fn read_report(dir: &Path, result: &Path, root: &Path, exit: Option<i32>) -> Result<(Report, Vec<Snapshot>)> {
    let mut bytes = Vec::new();
    files::open_regular(result)?.take(32_769).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 32_768, "local task report is too large");
    let mut report: Report = serde_json::from_slice(&bytes)?;
    ensure!(
        !report.summary.trim().is_empty() && report.summary.len() <= 16_384 && report.evidence.len() <= 8,
        "invalid local task result length"
    );
    if exit != Some(0) && matches!(report.status, Outcome::Passed | Outcome::NeedsReview) {
        report.status = Outcome::Failed;
        report.summary = "The local task exited unsuccessfully.".into();
    }
    let canonical = dir.canonicalize()?;
    let mut snapshots = Vec::new();
    let mut missing = 0;
    for name in &report.evidence {
        let source = dir.join(name);
        if Path::new(name).components().all(|part| matches!(part, Component::Normal(_)))
            && let Ok(path) = source.canonicalize()
            && path.starts_with(&canonical)
            && let Some(source_path) = path.to_str()
            && let Some(display) = source.file_name().and_then(|n| n.to_str())
            && let Ok(snapshot) = files::snapshot(root, dir, source_path, Some(display), &AtomicBool::new(false))
        {
            snapshots.push(snapshot);
            continue;
        }
        missing += 1;
    }
    if missing > 0 {
        let _ = write!(report.summary, "\n\n{missing} evidence file(s) could not be published.");
        if matches!(report.status, Outcome::Passed) {
            report.status = Outcome::NeedsReview;
        }
    }
    Ok((report, snapshots))
}

/// 遥测只记录终态类别，任务标题、命令、证据和路径均不出 Host。
fn metric(hub: &Hub, outcome: &str, category: Option<&str>) {
    crate::analytics::capture(
        hub,
        crate::analytics::Event::GroupLocalTaskFinished,
        serde_json::Map::from_iter([("outcome".into(), json!(outcome)), ("category".into(), json!(category))]),
    );
}

#[cfg(test)]
mod tests;
