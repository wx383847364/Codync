//! Group chats as the hub keeps them: what a busy one shows, and the groups a deleted bot leaves.

use super::{BotStatus, Hub, Runtime};
use crate::LockExt;
use crate::store::BotConfig;
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

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
            let base = cfg
                .group_session_base
                .take()
                // 复制编号群时继续原序列。群改名后以用户当前看到的名字开始编号。
                .filter(|base| {
                    cfg.name
                        .strip_prefix(base)
                        .and_then(|suffix| suffix.strip_prefix(' '))
                        .is_some_and(|suffix| suffix.parse::<u64>().is_ok())
                })
                .unwrap_or_else(|| cfg.name.trim().to_owned());
            let rows = self.store.bots()?;
            let names: std::collections::HashSet<_> = rows.iter().map(|row| row.config.name.as_str()).collect();
            // 已删除群的编号仍占用。手工取过的名字只占用自身的编号。
            cfg.name = (1..=names.len() + 1)
                .map(|number| format!("{base} {number}"))
                .find(|name| !names.contains(name.as_str()))
                .ok_or_else(|| anyhow!("group session number exhausted"))?;
            cfg.group_session_base = Some(base);
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

#[cfg(test)]
mod tests {
    use crate::hub::Hub;
    use crate::remote::identity::Identity;
    use crate::store::{BotConfig, Store};
    use serde_json::json;

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
