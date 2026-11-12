//! The `bots` table: roster rows, sessions, read marks and unread counts.

use super::entries::NOT_REMOVED;
use super::model::{BotConfig, BotRow, EntryKind, LastMessage, message_text};
use super::{Store, logged, next_rev};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

/// Raw `bots` row before the config JSON is parsed.
type RawBot = (String, String, i64, bool, Option<String>, i64);

fn row_bot(r: &rusqlite::Row) -> rusqlite::Result<RawBot> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0, r.get(4)?, r.get(5)?))
}

/// A stored config that no longer parses is skipped (and logged) rather than failing every listing.
fn parse_bot((id, cfg, rev, deleted, session_id, read_rev): RawBot) -> Option<BotRow> {
    match serde_json::from_str(&cfg) {
        Ok(config) => Some(BotRow { config, rev, deleted, session_id, read_rev }),
        Err(error) => {
            tracing::warn!(bot = %id, %error, "skipping unreadable bot config");
            None
        }
    }
}

const BOT_COLS: &str = "id, config, rev, deleted, session_id, read_rev";
const SAVE_BOT: &str = "INSERT INTO bots(id, rev, config) VALUES(?1, ?2, ?3)
     ON CONFLICT(id) DO UPDATE SET rev = ?2, config = ?3 WHERE deleted = 0";

impl Store {
    pub fn bots(&self) -> Result<Vec<BotRow>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {BOT_COLS} FROM bots"))?;
        let rows = st.query_map([], row_bot)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().filter_map(parse_bot).collect())
    }

    pub fn bot(&self, id: &str) -> Result<Option<BotRow>> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {BOT_COLS} FROM bots WHERE id = ?1"), [id], row_bot).optional()?;
        Ok(row.and_then(parse_bot))
    }

    pub fn save_bot(&self, cfg: &BotConfig) -> Result<i64> {
        let c = self.db.locked();
        let rev = next_rev(&c)?;
        c.execute(SAVE_BOT, params![cfg.id, rev, serde_json::to_string(cfg)?])?;
        Ok(rev)
    }

    /// 群成员更新、移除成员相关投递的取消及卡片必须一起提交，失败保留原成员列表。
    pub fn update_bot(&self, cfg: &BotConfig) -> Result<(Vec<super::deliveries::Delivery>, Vec<super::Entry>)> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let before: String =
            tx.query_row("SELECT config FROM bots WHERE id=?1 AND deleted=0", [&cfg.id], |r| r.get(0))?;
        let before: BotConfig = serde_json::from_str(&before)?;
        let rev = next_rev(&tx)?;
        tx.execute(SAVE_BOT, params![cfg.id, rev, serde_json::to_string(cfg)?])?;
        let mut cancelled = Vec::new();
        let mut cards = Vec::new();
        if cfg.is_group() {
            for member in before.members.iter().filter(|member| !cfg.members.contains(member)) {
                let (rows, entries) =
                    super::deliveries::cancel_in_transaction(&tx, &cfg.id, Some(member), "成员已移除")?;
                cancelled.extend(rows);
                cards.extend(entries);
            }
        }
        tx.commit()?;
        Ok((cancelled, cards))
    }

    /// Saves several bots in one transaction, each with its own rev: all of them or, on an error, none.
    pub fn save_bots(&self, cfgs: &[BotConfig]) -> Result<()> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        for cfg in cfgs {
            let rev = next_rev(&tx)?;
            tx.execute(SAVE_BOT, params![cfg.id, rev, serde_json::to_string(cfg)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn touch_bot(&self, id: &str) -> Result<i64> {
        let c = self.db.locked();
        let rev = next_rev(&c)?;
        c.execute("UPDATE bots SET rev = ?2 WHERE id = ?1", params![id, rev])?;
        Ok(rev)
    }

    /// 删除标记、相关投递取消和存活群卡片在同一事务提交，返回提交后需要发布的变化。
    pub fn delete_bot(&self, id: &str) -> Result<(i64, Vec<super::deliveries::Delivery>, Vec<super::Entry>)> {
        let mut db = self.db.locked();
        let c = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rev = next_rev(&c)?;
        // 删除标记与取消持久投递同事务提交，封住取消后再次入队的窗口。
        c.execute("UPDATE bots SET rev = ?2, deleted = 1, session_id = NULL WHERE id = ?1", params![id, rev])?;
        let (cancelled, cards) = super::deliveries::cancel_for_deletion(&c, id)?;
        c.execute("DELETE FROM entries WHERE bot_id = ?1", [id])?;
        codync_mem::store::drop_bot(&c, id)?;
        // Its dreaming bookmark, notices' revisions and the record of its moved memory.
        c.execute("DELETE FROM kv WHERE k GLOB ?1", [format!("memory.*{id}*")])?;
        // 清理成员的所有独立会话，以及被删除群在其他成员名下的会话槽。
        c.execute(
            "DELETE FROM kv WHERE k GLOB ?1 OR k GLOB ?2",
            [format!("lane.session.{id}@*"), super::run_session_key("*", &super::group_session_slot(id, "*"))],
        )?;
        // Its fallback chain's place and each agent's parked session (`agent::bot::fallback::park`).
        c.execute(
            "DELETE FROM kv WHERE k GLOB ?1 OR k GLOB ?2",
            [format!("fallback.*.{id}"), format!("fallback.*.{id}.*")],
        )?;
        // ponytail: snapshots of replaced sessions stay until the bot goes; prune by session if kv grows.
        c.execute(
            "DELETE FROM kv WHERE k GLOB ?1 OR k GLOB ?2",
            [format!("context.{id}.*"), format!("context.epoch.{id}.*")],
        )?;
        // Its artifacts' details (`chat::artifacts`).
        c.execute("DELETE FROM kv WHERE k GLOB ?1", [format!("artifact.{id}.*")])?;
        c.commit()?;
        Ok((rev, cancelled, cards))
    }

    /// 新建主会话时一次提交主会话与parked会话清理；失败不允许actor清除旧内存会话。
    pub fn reset_main_session(&self, bot: &str) -> Result<()> {
        let mut c = self.db.locked();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("UPDATE bots SET session_id=NULL WHERE id=?", [bot])?;
        let prefix = format!("fallback.session.{bot}.");
        tx.execute("DELETE FROM kv WHERE substr(k,1,length(?1))=?1", [prefix])?;
        let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0) FROM entries WHERE bot_id=?", [bot], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=?2",
            params![format!("fallback.fresh.{bot}"), seq.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 只有群和成员仍存在时才保存群会话，避免删除与正在打开的会话交错后留下记录。
    pub(crate) fn save_slotted_session(&self, bot: &str, slot: &str, sid: &str) -> Result<()> {
        let key = super::run_session_key(bot, slot);
        let Some(room) = super::slotted_group(slot) else {
            return self.kv_set(&key, sid);
        };
        let c = self.db.locked();
        c.execute(
            "INSERT INTO kv(k,v) SELECT ?1,?2 WHERE
             EXISTS (SELECT 1 FROM bots WHERE id=?3 AND deleted=0)
             AND EXISTS (SELECT 1 FROM bots WHERE id=?4 AND deleted=0)
             ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![key, sid, bot, room],
        )?;
        Ok(())
    }

    pub fn set_session(&self, id: &str, session: Option<&str>) -> Result<()> {
        let c = self.db.locked();
        c.execute("UPDATE bots SET session_id = ?2 WHERE id = ?1", params![id, session])?;
        Ok(())
    }

    /// The chat read: bumps the bot's `read_rev`.
    pub fn mark_read(&self, id: &str) -> Result<i64> {
        let c = self.db.locked();
        let rev = next_rev(&c)?;
        c.execute("UPDATE bots SET read_rev = ?2, rev = ?2 WHERE id = ?1", params![id, rev])?;
        Ok(rev)
    }

    /// Final agent messages + pending permission cards newer than what the user has read. A reacted-to message
    /// doesn't count again when it changes: the user reacted, so they have seen it. A bot's reaction isn't a change
    /// either (`contentRev`, see `Hub::bot_react`). Nor is a reply on a voice call: the user heard it.
    pub fn unread(&self, id: &str, read_rev: i64) -> i64 {
        let c = self.db.locked();
        let n = c.query_row(
            "SELECT COUNT(*) FROM entries WHERE bot_id = ?1 AND
               ((kind = 'agent' AND json_extract(data, '$.final') = 1 AND json_extract(data, '$.call') IS NULL) OR
                (kind = 'permission' AND json_extract(data, '$.status') = 'pending')) AND
               json_extract(data, '$.reactions') IS NULL AND
               COALESCE(json_extract(data, '$.contentRev'), rev) > ?2",
            params![id, read_rev],
            |r| r.get(0),
        );
        logged("unread", n).unwrap_or(0)
    }

    /// Newest chat-visible line for the roster preview, with its author
    /// (`None` for the user; a group reply names the bot that wrote it). A voice call's messages show only behind
    /// its line, so they aren't it.
    pub fn last_message(&self, id: &str) -> Option<LastMessage> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!(
                "SELECT kind, data, updated_at FROM entries WHERE bot_id = ?1 AND
                   (kind = 'user' OR (kind = 'agent' AND json_extract(data, '$.final') = 1)) AND
                   json_extract(data, '$.call') IS NULL AND {NOT_REMOVED}
                 ORDER BY seq DESC LIMIT 1"
            ),
            [id],
            |r| {
                let kind: String = r.get(0)?;
                let data: String = r.get(1)?;
                let at: i64 = r.get(2)?;
                let data = serde_json::from_str::<Value>(&data).unwrap_or_default();
                // A files-only message previews as its file names.
                let text = message_text(&data);
                let author =
                    (kind != EntryKind::User.as_str()).then(|| data["author"].as_str().unwrap_or(id).to_owned());
                Ok(LastMessage { text, at, author })
            },
        );
        logged("last message", row.optional()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::model::{BotKind, EntryKind, Lane, Permission};
    use crate::store::tests::temp_store;

    #[test]
    fn rev_sync_and_unread() {
        let s = temp_store();
        let cfg = BotConfig {
            id: "b1".into(),
            kind: BotKind::Agent,
            members: vec![],
            group_session_base: None,
            name: "Rev".into(),
            description: String::new(),
            avatar_color: "blue".into(),
            avatar_shape: "blob".into(),
            backend: "claude".into(),
            command: None,
            cwd: "/tmp".into(),
            permission: Permission::Ask,
            mode: None,
            model: None,
            model_options: std::collections::BTreeMap::new(),
            fast_mode: false,
            fallbacks: vec![],
            self_fallbacks: true,
            pinned: false,
            lead: false,
            position: 0,
            hidden: false,
            section: None,
            notify: None,
            connectors: vec![],
            skills: vec![],
            computer: false,
            manage_team: false,
            offered_to_network: true,
            reachable_by_assistants: true,
            created_by: None,
            shared_memory: None,
            harness_memory: false,
            auto_name: false,
            created_at: 0,
        };
        // Several at once: each with its own rev, in order.
        let other = BotConfig { id: "b0".into(), ..cfg.clone() };
        let before = s.current_rev();
        s.save_bots(&[other.clone(), other]).unwrap();
        assert_eq!(s.current_rev(), before + 2);
        assert_eq!(s.bot("b0").unwrap().map(|r| r.rev), Some(before + 2));
        s.save_bot(&cfg).unwrap();
        let u = s.insert_entry(&Lane::main("b1"), EntryKind::User, 1, &serde_json::json!({"text": "hi"})).unwrap();
        let a = s
            .insert_entry(&Lane::main("b1"), EntryKind::Agent, 1, &serde_json::json!({"text": "yo", "final": false}))
            .unwrap();
        assert!(a.rev > u.rev);
        assert_eq!(s.unread("b1", 0), 0, "narration is not unread");
        let a2 = s.update_entry(&a.id, &serde_json::json!({"text": "yo!", "final": true})).unwrap().unwrap();
        assert_eq!(s.unread("b1", 0), 1);
        assert_eq!(s.entries_since(a2.rev - 1, 500).unwrap().len(), 1);
        assert_eq!(s.entries_since(0, 1).unwrap().len(), 1, "fresh sync is capped per bot");
        assert_eq!(s.last_message("b1").unwrap().text, "yo!");
        let r = s.mark_read("b1").unwrap();
        assert_eq!(s.unread("b1", r), 0);
        assert_eq!(s.history("b1", a.seq, 10).unwrap().len(), 1);
        assert_eq!(s.bot("b1").unwrap().unwrap().config.permission, Permission::Ask);
        assert!(s.bot("missing").unwrap().is_none());
        // A voice call's messages show only behind its line: neither preview nor unread.
        let said = serde_json::json!({"text": "said on a call", "call": "c"});
        s.insert_entry(&Lane::main("b1"), EntryKind::User, 2, &said).unwrap();
        let heard = serde_json::json!({"text": "heard on it", "final": true, "call": "c"});
        s.insert_entry(&Lane::main("b1"), EntryKind::Agent, 2, &heard).unwrap();
        assert_eq!((s.unread("b1", r), s.last_message("b1").unwrap().text.as_str()), (0, "yo!"));

        let reply = serde_json::json!({"text": "done", "final": true});
        // A deleted bot stays deleted, and its winding-down actor leaves no lines behind.
        s.kv_set("fallback.session.b1.codex", "s1").unwrap();
        s.kv_set("fallback.on.b1", "codex").unwrap();
        s.kv_set("fallback.on.b10", "codex").unwrap();
        s.delete_bot("b1").unwrap();
        assert_eq!((s.kv_get("fallback.session.b1.codex"), s.kv_get("fallback.on.b1")), (None, None));
        assert!(s.kv_get("fallback.on.b10").is_some(), "another bot's");
        assert!(s.insert_entry(&Lane::main("b1"), EntryKind::Notice, 3, &reply).is_err());
        s.save_bot(&cfg).unwrap();
        assert!(s.bot("b1").unwrap().unwrap().deleted);
    }
}
