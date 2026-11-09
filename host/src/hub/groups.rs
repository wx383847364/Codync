//! Group chats as the hub keeps them: what a busy one shows, and the groups a deleted bot leaves.

use super::{BotStatus, Hub, Runtime};
use crate::LockExt;
use crate::store::BotConfig;
use anyhow::Result;

impl Hub {
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
