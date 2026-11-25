//! Group chats as the hub keeps them: what a busy one shows, and the groups a deleted bot leaves.

use super::{BotStatus, Hub, Runtime};
use crate::LockExt;
use crate::store::BotConfig;
use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const PURGE_PENDING: &str = "group.purge.pending.";

/// 新会话的显式成员选择。省略整个对象时沿用来源群配置。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupSessionSelection {
    members: Vec<String>,
    group_leader_id: Option<String>,
}

impl Hub {
    /// 从来源群开启独立会话，编号与确认后的成员配置在同一锁内保存。
    pub(crate) fn create_group_session(&self, id: &str, selection: Option<GroupSessionSelection>) -> Result<Value> {
        let bot = {
            // 在同一把锁下读取、分配编号、保存和广播，避免多端创建同名或看到过期名单。
            let _guard = self.emit_lock.locked();
            let mut cfg = self
                .store
                .bot(id)?
                .filter(|row| !row.deleted && row.config.is_group())
                .ok_or_else(|| anyhow!("group not found"))?
                .config;
            let (base, name) = self.group_session_name(&cfg)?;
            cfg.name = name;
            cfg.group_session_base = Some(base);
            cfg.group_session_owned_members.clear();
            cfg.stage = None;
            // 旧客户端没有提交选择。新客户端可明确选择空负责人或调整成员。
            let explicit = selection.is_some();
            if let Some(selection) = selection {
                cfg.members = selection.members;
                cfg.group_leader_id = selection.group_leader_id;
            }
            cfg.id = uuid::Uuid::new_v4().to_string();
            cfg.created_at = crate::store::now_ms();
            cfg.position = self.new_bot_position()?;
            cfg.hidden = false;
            let members = crate::chat::group::valid_members(&self.store, &cfg.id, &cfg.members)?;
            ensure!(!explicit || members == cfg.members, "A member is no longer available. Remove it before saving.");
            cfg.members = members;
            if explicit {
                ensure!(
                    cfg.group_leader_id.as_ref().is_none_or(|id| cfg.members.contains(id)),
                    "Choose a lead from the selected members."
                );
            }
            cfg.clear_departed_group_leader();
            self.store.save_bot(&cfg)?;
            let bot = self.bot_value(&cfg.id).ok_or_else(|| anyhow!("new group could not be read"))?;
            let _ = self.events.send(json!({"type":"bot","rev":bot["rev"],"bot":bot}));
            bot
        };
        crate::network::publish(self);
        crate::usage::bots_changed(self);
        Ok(bot)
    }

    /// 复用原群的编号序列，独立创建和旧接口采用同一套名称分配。
    pub(super) fn group_session_name(&self, cfg: &BotConfig) -> Result<(String, String)> {
        let base = cfg
            .group_session_base
            .clone()
            // 复制编号群时继续原序列。群改名后以用户当前看到的名字开始编号。
            .filter(|base| {
                cfg.name
                    .strip_prefix(base)
                    .and_then(|suffix| suffix.strip_prefix(' '))
                    .is_some_and(|suffix| suffix.parse::<u64>().is_ok())
            })
            .unwrap_or_else(|| cfg.name.trim().to_owned());
        let rows = self.store.bots()?;
        let names: HashSet<_> = rows.iter().map(|row| row.config.name.as_str()).collect();
        // 已删除群的编号仍占用。手工取过的名字只占用自身的编号。
        let name = (1..=names.len() + 1)
            .map(|number| format!("{base} {number}"))
            .find(|name| !names.contains(name.as_str()))
            .ok_or_else(|| anyhow!("group session number exhausted"))?;
        Ok((base, name))
    }

    /// 只删除来源群和本群独用的成员。引用同一 ID 的其他群和独立副本继续可用。
    pub(crate) async fn purge_group(self: &Arc<Self>, id: &str) -> Result<()> {
        let _purge = self.group_purge.lock().await;
        let key = format!("{PURGE_PENDING}{id}");
        let ids = {
            let _emit = self.emit_lock.locked();
            if let Some(saved) = self.store.kv_read(&key)? {
                // 事务已提交时沿用原范围，不重新读取已清空的群和成员配置。
                serde_json::from_str::<Vec<String>>(&saved)?
            } else {
                let rows = self.store.bots()?;
                let group =
                    rows.iter().find(|row| row.config.id == id && row.config.is_group()).context("group not found")?;
                if group.deleted {
                    return Ok(());
                }
                let shared: HashSet<_> = rows
                    .iter()
                    .filter(|row| !row.deleted && row.config.is_group() && row.config.id != id)
                    .flat_map(|row| group_purge_members(&row.config))
                    .collect();
                let candidates: HashSet<_> = group_purge_members(&group.config).collect();
                let mut ids = vec![id.to_owned()];
                ids.extend(rows.iter().filter_map(|row| {
                    // 已从群移除的专属角色也属于本群。独立复制使用新 ID，不会进入此范围。
                    let cfg = &row.config;
                    (!cfg.is_group() && !shared.contains(&cfg.id) && candidates.contains(&cfg.id))
                        .then(|| cfg.id.clone())
                }));
                for (id, deleted) in ids.iter().zip(self.store.purge_group(&ids, &key)?) {
                    let rev = deleted.rev;
                    let _ = self.events.send(json!({"type":"bot","rev":rev,"bot":{"id":id,"deleted":true,"rev":rev}}));
                    self.delivery_entries(&deleted.cards);
                    self.stop_delivery_turns(&deleted.cancelled);
                }
                ids
            }
        };

        let mut tasks = Vec::new();
        for id in &ids {
            self.team.end_turn(id);
            if let Some(handle) = self.bots.locked().remove(id) {
                let _ = handle.tx.send(crate::agent::bot::Cmd::Shutdown);
                tasks.push(handle.task);
            }
            self.dev_servers.forget_bot(&self.store, id);
        }
        // 等执行者退出后再清盘，防止关闭期间把附件、私有内存或工作区重新写出来。
        // 同时等待各个 actor，超时后结束尚未退出的任务。
        futures::future::join_all(tasks.into_iter().map(|mut task| async move {
            if tokio::time::timeout(Duration::from_secs(5), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }))
        .await;
        {
            let mut runtime = self.runtime.locked();
            for id in &ids {
                runtime.remove(id);
            }
        }
        crate::network::publish(self);
        crate::usage::bots_changed(self);
        let hub = self.clone();
        tokio::task::spawn_blocking(move || hub.finish_group_purge(&key, &ids))
            .await?
            .context("Group deleted. Resource cleanup will be retried when the host restarts.")
    }

    /// 进程中断后补做已提交删除的清盘，失败仍留待下一次启动或重试。
    pub(super) fn resume_group_purges(&self) {
        for key in self.store.kv_prefix(PURGE_PENDING) {
            let result = (|| -> Result<()> {
                let Some(saved) = self.store.kv_read(&key)? else { return Ok(()) };
                self.finish_group_purge(&key, &serde_json::from_str::<Vec<String>>(&saved)?)
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "couldn't finish deleting a group's resources");
            }
        }
    }

    /// 删除 Host 专属目录；仍被存活 Bot 使用的工作区保存下来。
    fn finish_group_purge(&self, key: &str, ids: &[String]) -> Result<()> {
        for id in ids {
            self.routines.purge_bot(&self.store, id)?;
        }
        // 等正在记录的私有记忆落盘后再清理。先释放记忆锁，避免与发布锁反向获取。
        crate::chat::memory::files::wait_for_writers();
        let staged = self.detach_purged_folders(ids)?;
        for path in staged {
            remove_purged_folder(&path)?;
        }
        self.store.finish_group_purge(ids, key)
    }

    /// 在配置发布锁内断开旧目录，只持锁完成检查和同层重命名。递归删除在锁外执行。
    fn detach_purged_folders(&self, ids: &[String]) -> Result<Vec<PathBuf>> {
        let _emit = self.emit_lock.locked();
        let workspaces: Vec<_> = self
            .store
            .bots()?
            .into_iter()
            .filter(|row| !row.deleted && !row.config.cwd.is_empty())
            .map(|row| resolved_path(Path::new(&row.config.cwd)))
            .collect();
        let mut staged = Vec::new();
        for id in ids {
            let mut components = Path::new(id).components();
            ensure!(
                matches!(components.next(), Some(Component::Normal(part)) if part == id.as_str())
                    && components.next().is_none(),
                "invalid bot storage ID"
            );
            let root = crate::service::data_dir().join("bots").join(id);
            let resolved_root = resolved_path(&root);
            let detached = root.with_file_name(format!(".purging-{id}"));
            // 上次执行可能在改名后中断，即使共享项目配置已变更也先认领旧暂存目录。
            staged.push(detached.clone());
            if workspaces.iter().any(|path| path.starts_with(&resolved_root)) {
                // 整棵目录若是存活项目使用的链接，保留链接入口。
                if root.is_symlink() {
                    continue;
                }
                for name in ["files", "artifacts", "uploads", "bot-memory", "local-task-runs"] {
                    let path = root.join(name);
                    let resolved = resolved_path(&path);
                    let destination = path.with_file_name(format!(".purging-{name}"));
                    if !workspaces
                        .iter()
                        .any(|workspace| workspace == &resolved_root || workspace.starts_with(&resolved))
                    {
                        detach_purged_folder(&path, &destination)?;
                    }
                    staged.push(destination);
                }
            } else {
                detach_purged_folder(&root, &detached)?;
            }
        }
        Ok(staged)
    }

    /// A group is busy while one of its members works in it: that member's status and activity.
    /// 群状态投影来源成员执行及保存故障，成员交接期间保留调度状态。
    pub fn group_runtime(&self, group: &BotConfig) -> Runtime {
        let busy = {
            let runtime = self.runtime.locked();
            group
                .members
                .iter()
                // 只投影正在本群执行的成员，别处的工作与审批留在原聊天。
                .filter_map(|m| {
                    let r = runtime.get(m)?;
                    (r.status != BotStatus::Idle && r.lane.as_ref().is_some_and(|l| l.chat == group.id))
                        .then_some((m, r))
                })
                // 不同消息可同时执行，优先展示待审批成员，同等状态仍选名单中第一位。
                .min_by_key(|(_, r)| r.status != BotStatus::NeedsInput)
                // 仅复制选中成员的状态，后续格式化可以在锁外完成。
                .map(|(member, r)| (member, r.clone()))
        };
        let Some((member, mut r)) = busy else {
            // runner 在成员之间交接时仍是工作中，避免短暂 idle 使客户端误判整轮结束。
            if self.groups.running(&crate::store::Lane::main(&group.id)) {
                return Runtime {
                    status: BotStatus::Working,
                    activity: "Scheduling group deliveries…".into(),
                    ..Runtime::default()
                };
            }
            return Runtime::default();
        };
        let name = self.bot_name(member);
        r.activity = if r.activity.is_empty() { name } else { format!("{name}: {}", r.activity) };
        r
    }

    /// Takes bot `id` out of every group it was in; what it said there stays. Each group is read again
    /// under the lock, so an edit or a section rename since this scan isn't written back.
    pub(super) fn leave_groups(&self, id: &str) -> Result<()> {
        let groups: Vec<String> =
            self.store.bots()?.into_iter().filter(|r| !r.deleted && r.config.is_group()).map(|r| r.config.id).collect();
        for group in groups {
            let left = {
                let _g = self.emit_lock.locked();
                match self.store.bot(&group)?.filter(|r| !r.deleted) {
                    Some(mut row) if row.config.members.iter().any(|m| m == id) => {
                        row.config.members.retain(|m| m != id);
                        row.config.clear_departed_group_leader();
                        self.store.save_bot(&row.config)?;
                        true
                    }
                    _ => false,
                }
            };
            if left {
                self.emit_bot(&group);
            }
        }
        Ok(())
    }
}

/// 当前成员和创建时拥有的角色一致参与归属判断，已移除的专属角色仍归其原群。
fn group_purge_members(group: &BotConfig) -> impl Iterator<Item = &String> {
    let owned = group.stage.as_ref().map_or(&group.group_session_owned_members, |stage| &stage.owned_members);
    group.members.iter().chain(owned)
}

/// 比较实际路径，目录暂不存在时仍保留配置中的路径约束。
fn resolved_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// 确定的暂存名使进程在改名后中断时仍能找到待删目录。
fn detach_purged_folder(path: &Path, destination: &Path) -> Result<()> {
    match std::fs::rename(path, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("couldn't detach group resources"),
    }
}

/// 已清理目录允许重试，其他磁盘错误返回给调用方并保留清理记录。
fn remove_purged_folder(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("couldn't delete group resources"),
    }
}

#[cfg(test)]
mod tests {
    use crate::hub::Hub;
    use crate::remote::identity::Identity;
    use crate::store::{BotConfig, Store};
    use serde_json::json;

    /// 独立群仍拥有已移除的复制角色；符号链接复用的工作区也要保留。
    #[cfg(unix)]
    #[tokio::test]
    async fn purge_keeps_another_groups_owned_role_and_its_workspace_alias() {
        use crate::agent::workspace;
        use std::os::unix::fs::symlink;

        let id = uuid::Uuid::new_v4().to_string();
        let workspace = workspace::path(&id);
        let dir = std::env::temp_dir().join(format!("codync-purge-alias-{id}"));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(workspace.join("work.txt"), "shared project").unwrap();
        let alias = dir.join("project");
        symlink(&workspace, &alias).unwrap();
        let hub = Hub::new(
            Store::open(std::path::Path::new(":memory:")).unwrap(),
            "groups-test".into(),
            Identity::load_or_create(&dir).unwrap(),
            "test".into(),
            0,
        );
        for config in [
            json!({"id":id,"name":"Source bot","backend":"opencode","cwd":workspace}),
            json!({"id":"copy","name":"Copied bot","backend":"opencode","cwd":alias}),
            json!({"id":"source","name":"Source","kind":"group","members":[id,"copy"]}),
            json!({"id":"other","name":"Other","kind":"group","groupSessionOwnedMembers":["copy"]}),
        ] {
            hub.store.save_bot(&serde_json::from_value::<BotConfig>(config).unwrap()).unwrap();
        }
        hub.purge_group("source").await.unwrap();
        assert!(hub.store.bot(&id).unwrap().unwrap().deleted);
        assert!(!hub.store.bot("copy").unwrap().unwrap().deleted);
        assert!(!hub.store.bot("other").unwrap().unwrap().deleted);
        assert_eq!(std::fs::read_to_string(alias.join("work.txt")).unwrap(), "shared project");
        std::fs::remove_dir_all(workspace.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 同时复制来源群时分别提交编号，新群仍以来源成员顺序和设置打开。
    #[tokio::test]
    async fn concurrent_group_sessions_keep_members_and_get_distinct_names() {
        let dir = std::env::temp_dir().join(format!("codync-groups-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "groups-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        for v in [
            json!({"id":"a","name":"a","backend":"claude"}),
            json!({"id":"b","name":"Crew 99","backend":"claude"}),
            json!({"id":"g","name":"Crew","kind":"group","members":["b","a"],"description":"Release","section":"Work"}),
        ] {
            hub.store.save_bot(&serde_json::from_value::<BotConfig>(v).unwrap()).unwrap();
        }
        let create = || {
            // 两个阻塞请求同时提交，必须各自得到新的群记录。
            let hub = hub.clone();
            tokio::task::spawn_blocking(move || hub.create_group_session("g", None).unwrap())
        };
        let (a, b) = tokio::join!(create(), create());
        let groups = [a.unwrap(), b.unwrap()];
        let mut names = groups.each_ref().map(|group| group["name"].as_str().unwrap());
        names.sort_unstable();
        assert_eq!(names, ["Crew 1", "Crew 2"]);
        assert_ne!(groups[0]["id"], groups[1]["id"]);
        for group in &groups {
            assert_eq!(group["members"], json!(["b", "a"]));
            assert_eq!(group["description"], "Release");
            assert_eq!(group["section"], "Work");
            assert!(hub.store.history(group["id"].as_str().unwrap(), i64::MAX, 10).unwrap().is_empty());
        }
        assert_eq!(hub.create_group_session(groups[0]["id"].as_str().unwrap(), None).unwrap()["name"], "Crew 3");
        assert_eq!(
            crate::chat::group::with_members(&hub.store, &["a".into(), "b".into()], None).unwrap().as_deref(),
            Some("g")
        );
        assert!(hub.create_group_session("a", None).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 删除成员后，存活群的queued卡片与取消终态一起发布，重连也能读到同一状态。
    #[tokio::test]
    async fn a_deleted_bot_leaves_its_groups_and_they_keep_their_latest_section() {
        let dir = std::env::temp_dir().join(format!("codync-groups-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "groups-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        for v in [
            json!({"id": "a", "name": "a", "backend": "claude", "cwd": "/tmp"}),
            json!({"id": "b", "name": "b", "backend": "claude", "cwd": "/tmp"}),
            json!({"id": "g", "name": "g", "kind": "group", "members": ["a", "b"], "section": "Work"}),
        ] {
            hub.store.save_bot(&serde_json::from_value::<BotConfig>(v).unwrap()).unwrap();
        }
        hub.rename_section("Work", Some("Play")).unwrap();
        let source = hub.post_group("g", &json!({"text":"queued work"}), &["a".into()]).unwrap();
        let mut events = hub.events.subscribe();
        hub.delete_bot("a").unwrap();
        let id = format!("delivery-card-{}-1", source.id);
        assert_eq!(hub.store.entry(&id).unwrap().data["deliveryRows"][0]["status"], "cancelled");
        let mut published = false;
        let mut previous = 0;
        while let Ok(event) = events.try_recv() {
            if let Some(rev) = event["rev"].as_i64() {
                assert!(rev >= previous);
                previous = rev;
            }
            if event["type"] == "entry" && event["entry"]["id"] == id {
                assert_eq!(event["entry"]["data"]["deliveryRows"][0]["status"], "cancelled");
                published = true;
            }
        }
        assert!(published, "online clients receive the cancelled card immediately");
        let group = hub.store.bot("g").unwrap().unwrap().config;
        assert_eq!((group.members, group.section.as_deref()), (vec!["b".to_owned()], Some("Play")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
