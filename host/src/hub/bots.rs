//! Creating, changing and deleting bots.

use super::{Hub, patch, sections, validate};
use crate::LockExt;
use crate::agent::bot::Cmd;
use crate::store::BotConfig;
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::sync::Arc;

impl Hub {
    /// 新建普通机器人或普通群；阶段身份由单独的创建入口分配。
    pub fn create_bot(self: &Arc<Self>, mut cfg: BotConfig) -> Result<Value> {
        cfg.group_session_base = None;
        cfg.stage = None;
        cfg.id = uuid::Uuid::new_v4().to_string();
        cfg.created_at = crate::store::now_ms();
        cfg.position = self.new_bot_position()?;
        // No name yet (Grok Bot's flow): it gets one after its first few conversations.
        cfg.auto_name = !cfg.is_group() && cfg.name.trim().is_empty();
        if cfg.auto_name {
            crate::chat::naming::PLACEHOLDER.clone_into(&mut cfg.name);
        }
        if !cfg.is_group() {
            cfg.group_leader_id = None;
        }
        crate::agent::fallback::tidy(&mut cfg);
        cfg.section = sections::normalize_section(cfg.section.as_deref())?;
        validate(&cfg)?;
        crate::agent::workspace::prepare(&mut cfg)?;
        {
            let _g = self.emit_lock.locked();
            if cfg.is_group() {
                cfg.members = crate::chat::group::valid_members(&self.store, &cfg.id, &cfg.members)?;
                ensure!(
                    cfg.group_leader_id.as_ref().is_none_or(|id| cfg.members.contains(id)),
                    "Choose a lead from the selected members."
                );
                // 名单与负责人一致时打开既有群，避免改变已有会话的默认接收人。
                if let Some(existing) =
                    crate::chat::group::with_members(&self.store, &cfg.members, cfg.group_leader_id.as_deref())?
                {
                    return Ok(self.bot_value(&existing).unwrap_or(Value::Null));
                }
            }
            self.store.save_bot(&cfg)?;
        }
        crate::network::publish(self);
        if !cfg.is_group() {
            self.spawn_bot(cfg.clone());
            crate::market::agent_sign_in::ask_before_first_message(self, &cfg);
        }
        self.emit_bot(&cfg.id);
        crate::usage::bots_changed(self);
        Ok(self.bot_value(&cfg.id).unwrap_or(Value::Null))
    }

    /// 更新配置与取消被移除成员投递原子提交，再发布卡片和定向停止通知。
    pub fn update_bot(&self, patch: &Value) -> Result<Value> {
        let id = patch["id"].as_str().ok_or_else(|| anyhow!("id required"))?.to_owned();
        // Read, merge and save under one lock: a section rename in between would otherwise be written back.
        let (before, cfg) = {
            let _g = self.emit_lock.locked();
            let row = self.store.bot(&id)?.filter(|b| !b.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            let mut cfg = patch::patched(&row.config, patch)?;
            if cfg.is_group() {
                cfg.members = crate::chat::group::valid_members(&self.store, &cfg.id, &cfg.members)?;
                cfg.clear_departed_group_leader();
            }
            crate::agent::fallback::tidy(&mut cfg);
            cfg.section = sections::normalize_section(cfg.section.as_deref())?;
            // ponytail: validated under the lock, so a workspace folder on a hung network share (its
            // `is_dir`) stalls every bot's events until it answers. Upgrade: build and validate outside the
            // lock from a snapshot, then save under it only if the stored config is unchanged (compare the
            // serialized configs, not rev), else retry.
            validate(&cfg)?;
            crate::agent::workspace::prepare(&mut cfg)?;
            crate::agent::own_memory::settle(&mut cfg, &row.config, patch);
            let (cancelled, cards) = self.store.update_bot(&cfg)?;
            // 配置 rev 早于取消卡片；先发布它，断线游标不能越过尚未发送的成员变更。
            if let Some(bot) = self.bot_value(&id) {
                let _ = self.events.send(json!({"type":"bot","rev":bot["rev"],"bot":bot}));
            }
            self.delivery_entries(&cards);
            self.stop_cancelled_deliveries(&cancelled);
            (row.config, cfg)
        };
        crate::network::publish(self);
        if !cfg.is_group() {
            if crate::chat::memory::shared(&cfg) != crate::chat::memory::shared(&before) {
                crate::chat::memory::changed(self, Some(&id));
            }
            self.send_cmd(&id, Cmd::Reconfigure(Box::new(cfg)))?;
        }
        self.emit_bot(&id);
        crate::usage::bots_changed(self);
        Ok(self.bot_value(&id).unwrap_or(Value::Null))
    }

    /// 删除标记与取消投递先提交，再关闭 actor 和清理成员资源。
    pub fn delete_bot(&self, id: &str) -> Result<()> {
        self.routines.disable_bot(&self.store, id)?;
        // 标记删除和取消先提交，再停止执行，不能让旧actor在删除窗口接受新工作。
        {
            let _g = self.emit_lock.locked();
            let (rev, cancelled, cards) = self.store.delete_bot(id)?;
            let _ = self.events.send(json!({"type":"bot","rev":rev,"bot":{"id":id,"deleted":true,"rev":rev}}));
            self.delivery_entries(&cards);
            self.stop_cancelled_deliveries(&cancelled);
        }
        self.team.end_turn(id);
        let _ = self.send_cmd(id, Cmd::Shutdown);
        self.bots.locked().remove(id);
        self.runtime.locked().remove(id);
        self.dev_servers.forget_bot(&self.store, id);
        crate::network::publish(self);
        // Its shared files and artifacts are unreachable once the bot is deleted.
        let folders = [crate::chat::files::root(id), crate::chat::artifacts::root(id)];
        let bot = id.to_owned();
        tokio::task::spawn_blocking(move || {
            crate::chat::memory::files::remove_bot(&bot);
            for folder in folders {
                if let Err(error) = std::fs::remove_dir_all(&folder)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(error = %error, "couldn't remove a deleted bot's shared files or artifacts");
                }
            }
        });
        crate::usage::bots_changed(self);
        self.leave_groups(id)
    }
}
