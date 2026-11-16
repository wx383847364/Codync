//! 群成员的实时上下文、显示基准和主动新开会话请求。

use super::handoff::{GroupHandoff, Phase, active_session, archive_context, private_key, projection};
use super::{Status, completion};
use crate::LockExt;
use crate::store::{BotConfig, Entry, EntryKind, Store, group_session_slot};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_BASIS: u64 = 512_000;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct MemberSessionRequest {
    pub room: String,
    pub generation: String,
    #[serde(default)]
    pub retirement_pending: bool,
    #[serde(default)]
    pub needs_shorter_summary: bool,
    pub handoff: GroupHandoff,
}

#[derive(Clone)]
pub(crate) struct MemberContext {
    pub shared: bool,
    pub backend: String,
    pub model: Option<String>,
    pub slot: Option<String>,
    pub session: Option<String>,
    pub generation: Option<String>,
    pub used: Option<u64>,
    pub size: Option<u64>,
}

/// 共享主会话的主动请求按成员唯一，独立会话按成员和群唯一。
fn request_key(bot: &str, room: Option<&str>) -> String {
    format!("group.member-session.{bot}.{}", room.unwrap_or("main"))
}

/// 用量绑定后端和会话槽，避免执行器复用会话 ID 时串到另一个聊天。
fn usage_key(bot: &str, backend: &str, slot: Option<&str>) -> String {
    format!("session.scoped-usage.{bot}.{backend}.{}", slot.unwrap_or("main"))
}

/// 原派工交接在其事务中也能核对执行器的内部会话版本。
pub(super) fn scoped_generation(
    c: &Connection,
    bot: &str,
    backend: &str,
    slot: Option<&str>,
    sid: &str,
) -> Result<Option<String>> {
    let Some(snapshot) = kv(c, &usage_key(bot, backend, slot))? else { return Ok(None) };
    let snapshot: Value = serde_json::from_str(&snapshot)?;
    Ok(if snapshot["session"].as_str() == Some(sid) {
        snapshot["generation"].as_str().map(str::to_owned)
    } else {
        None
    })
}

/// 同一个成员卡展示和主动请求读取同一份真实快照。
fn context(c: &Connection, room: &BotConfig, bot: &BotConfig) -> Result<MemberContext> {
    let shared = room.group_session_base.is_none() && room.stage.is_none();
    let current = kv(c, &format!("fallback.on.{}", bot.id))?;
    let step = current.as_deref().and_then(|agent| crate::agent::fallback::step_of(bot, agent)).unwrap_or(0);
    let effective = crate::agent::fallback::effective(bot, step);
    let slot = (!shared).then(|| group_session_slot(&room.id, &effective.backend));
    let session = active_session(c, &bot.id, slot.as_deref())?;
    let snapshot: Value = kv(c, &usage_key(&bot.id, &effective.backend, slot.as_deref()))?
        .map(|v| serde_json::from_str(&v))
        .transpose()?
        .unwrap_or_default();
    let matches = session.is_some() && session.as_deref() == snapshot["session"].as_str();
    let generation = matches.then(|| snapshot["generation"].as_str().map(str::to_owned)).flatten();
    let (used, size) =
        if matches { (snapshot["used"].as_u64(), snapshot["size"].as_u64().filter(|n| *n > 0)) } else { (None, None) };
    Ok(MemberContext {
        shared,
        backend: effective.backend,
        model: effective.model,
        slot,
        session,
        generation,
        used,
        size,
    })
}

/// 50% 用实际窗口判断，整数运算不受显示精度或档位影响。
impl MemberContext {
    pub fn recommend(&self) -> bool {
        self.used.zip(self.size).is_some_and(|(used, size)| used >= size.div_ceil(2))
    }
}

/// 群操作始终通过同一事务重新读取成员和阶段状态。
fn config(c: &Connection, id: &str) -> Result<BotConfig> {
    let raw: String = c
        .query_row("SELECT config FROM bots WHERE id=? AND deleted=0", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| anyhow!("Conversation no longer exists"))?;
    Ok(serde_json::from_str(&raw)?)
}

/// 用量读取支持已归档群，确认时另行检查该群能否工作。
fn member(c: &Connection, room: &str, bot: &str) -> Result<(BotConfig, MemberContext)> {
    let room = config(c, room)?;
    ensure!(room.is_group() && room.members.iter().any(|id| id == bot), "Bot is no longer a group member");
    let cfg = config(c, bot)?;
    Ok((room.clone(), context(c, &room, &cfg)?))
}

/// 允许在同一事务读取快照和活跃会话引用。
fn kv(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT v FROM kv WHERE k=?", [key], |r| r.get(0)).optional()
}

/// 卡片及确认共同使用同一份持久记录。
fn read_request(c: &Connection, key: &str) -> Result<Option<MemberSessionRequest>> {
    Ok(kv(c, key)?.map(|v| serde_json::from_str(&v)).transpose()?)
}

/// 普通群的交接标识只用于私聊定位。
fn private_id(r: &MemberSessionRequest) -> String {
    format!("group-member-handoff-{}", r.handoff.revision)
}

/// 保存主动交接并更新私聊卡，不把正文发到群。
fn save(c: &Connection, bot: &str, key: &str, r: &MemberSessionRequest) -> Result<Vec<Entry>> {
    c.execute(
        "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![key, serde_json::to_string(r)?],
    )?;
    if r.handoff.slot.is_some() {
        return Ok(Vec::new());
    }
    let room = config(c, &r.room).map_or_else(|_| r.room.clone(), |cfg| cfg.name);
    let (status, line) = match r.handoff.phase {
        Phase::Draft => ("draft", "Review your summary before starting a new session."),
        Phase::Approved => ("approved", "A new session will start after the current turn."),
        Phase::Switched | Phase::Sent => {
            ("sent", "New session ready. The summary goes into your next private message.")
        }
        Phase::Superseded => ("kept", "This handoff is no longer current."),
    };
    let turn = c.query_row("SELECT COALESCE(MAX(turn),0) FROM entries WHERE bot_id=?", [bot], |row| row.get(0))?;
    Ok(vec![completion::entry(
        c,
        &private_id(r),
        bot,
        EntryKind::Notice,
        turn,
        &json!({"text":format!("{room} · {line}"),"style":"info","memberSessionRoom":r.room,
            "handoff":{"status":status,"reason":"groupCapacity","text":r.handoff.text,"revision":r.handoff.revision}}),
    )?])
}

/// 查找最早仍因容量等待的任务，把动作保留在真实的派工身份上。
fn blocked(c: &Connection, room: &str, bot: &str, context: &MemberContext) -> Result<Option<Value>> {
    let id: Option<String> = c
        .query_row(
            "SELECT d.id FROM deliveries d JOIN kv ON k='group.handoff.' || d.id
         WHERE d.target_bot_id=?2 AND d.status IN ('queued','claimed','started')
           AND json_extract(v,'$.phase') IN ('draft','approved','switched')
           AND (d.room_id=?1 OR (?3 AND json_extract(v,'$.slot') IS NULL))
           AND json_extract(v,'$.backend')=?4
           AND (json_extract(v,'$.session')=?5 OR json_extract(v,'$.phase')='switched')
         ORDER BY (json_extract(v,'$.phase')='draft'), d.rowid LIMIT 1",
            params![room, bot, context.shared, context.backend, context.session],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    let Some(d) = super::get(c, &id)? else { return Ok(None) };
    let mut value = projection(c, &d)?;
    if value.is_null() && matches!(d.status, Status::Claimed | Status::Started) {
        return Ok(Some(json!({"phase":"approved","deliveryId":id})));
    }
    value["deliveryId"] = json!(id);
    Ok(Some(value))
}

/// 会话身份包含执行器、模型及槽位，草稿跨配置变化必须重新审阅。
fn same_session(r: &MemberSessionRequest, current: &MemberContext) -> bool {
    r.handoff.slot == current.slot
        && r.handoff.backend == current.backend
        && r.handoff.model == current.model
        && current.session.as_deref() == Some(&r.handoff.session)
        && current.generation.as_deref() == Some(&r.generation)
}

impl Store {
    /// 记录执行器实际汇报，保持缺报容量和整理后的未知用量。
    pub(crate) fn save_scoped_context(
        &self,
        bot: &str,
        backend: &str,
        slot: Option<&str>,
        sid: &str,
        used: Option<u64>,
        size: Option<u64>,
    ) -> Result<()> {
        let key = usage_key(bot, backend, slot);
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest: Value = kv(&tx, &key)?.map(|v| serde_json::from_str(&v)).transpose()?.unwrap_or_default();
        let generation = (latest["session"].as_str() == Some(sid))
            .then(|| latest["generation"].as_str())
            .flatten()
            .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
        tx.execute(
            "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![key, json!({"session":sid,"generation":generation,"used":used,"size":size}).to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 新开会话会更换内部版本，即使执行器复用了同一字符串 ID。
    pub(crate) fn reset_scoped_context(&self, bot: &str, backend: &str, slot: Option<&str>, sid: &str) -> Result<()> {
        self.kv_set(
            &usage_key(bot, backend, slot),
            &json!({
                "session":sid, "generation":uuid::Uuid::new_v4().to_string(), "used":null, "size":null
            })
            .to_string(),
        )
    }

    /// 为容量交接记录当前会话同槽、同后端的真实版本。
    pub(crate) fn scoped_context_generation(
        &self,
        bot: &str,
        backend: &str,
        slot: Option<&str>,
        sid: &str,
    ) -> Result<Option<String>> {
        scoped_generation(&self.db.locked(), bot, backend, slot, sid)
    }

    /// 读取一次快照，确认执行器 ID 与指定槽的最新身份相同。
    pub(crate) fn scoped_context(
        &self,
        bot: &str,
        backend: &str,
        slot: Option<&str>,
        sid: &str,
    ) -> Result<(Option<u64>, Option<u64>)> {
        let db = self.db.locked();
        let Some(raw) = kv(&db, &usage_key(bot, backend, slot))? else { return Ok((None, None)) };
        let snapshot: Value = serde_json::from_str(&raw)?;
        if snapshot["session"].as_str() == Some(sid) {
            Ok((snapshot["used"].as_u64(), snapshot["size"].as_u64()))
        } else {
            Ok((None, None))
        }
    }

    /// 主会话从当前执行器的槽读取，绝不回读独立群或例行任务的快照。
    pub(crate) fn main_context_usage(&self, bot: &BotConfig, sid: &str) -> Result<(Option<u64>, Option<u64>)> {
        let backend = self.kv_read(&format!("fallback.on.{}", bot.id))?.unwrap_or_else(|| bot.backend.clone());
        self.scoped_context(&bot.id, &backend, None, sid)
    }

    /// 一次快照同步上下文、显示基准和可操作状态，不含会话 ID 或摘要。
    pub(crate) fn group_session_contexts(&self, room_id: &str) -> Result<Value> {
        let c = self.db.locked();
        let room = config(&c, room_id)?;
        ensure!(room.is_group(), "Not a group conversation");
        let closed = crate::store::bots::stage_closed(&c, room_id)?;
        let basis = kv(&c, &format!("group.context-basis.{room_id}"))?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_BASIS);
        let mut members = Vec::new();
        for id in &room.members {
            let Ok(bot) = config(&c, id) else { continue };
            let usage = context(&c, &room, &bot)?;
            let proposal = read_request(&c, &request_key(id, (!usage.shared).then_some(room_id)))?;
            let pending = proposal.as_ref().filter(|r| same_session(r, &usage) && r.handoff.phase == Phase::Approved);
            let handoff = if let Some(pending) = pending {
                Some(json!({"phase":pending.handoff.phase,"shared":usage.shared}))
            } else if proposal.as_ref().is_some_and(|r| r.needs_shorter_summary && r.handoff.phase == Phase::Switched) {
                Some(json!({"phase":"draft","shared":false}))
            } else {
                blocked(&c, room_id, id, &usage)?
            };
            let recommended = usage.recommend();
            members.push(json!({"botId":id, "hasSession":usage.session.is_some(),"used":usage.used,"size":usage.size,
                "shared":usage.shared,"recommended":recommended && !closed,"handoff":handoff}));
        }
        Ok(json!({"basis":basis,"members":members}))
    }

    /// 按群持久化显示档位，三个档位都不影响模型预检。
    pub(crate) fn set_group_context_basis(&self, room: &str, basis: u64) -> Result<()> {
        ensure!(matches!(basis, 256_000 | 512_000 | 1_000_000), "Unknown display basis");
        let c = self.db.locked();
        ensure!(config(&c, room)?.is_group(), "Not a group conversation");
        c.execute(
            "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![format!("group.context-basis.{room}"), basis.to_string()],
        )?;
        Ok(())
    }

    /// 摘要生成前读取当前成员会话和最早待确认的原派工。
    pub(crate) fn group_member_context(&self, room: &str, bot: &str) -> Result<(MemberContext, Option<Value>)> {
        let c = self.db.locked();
        ensure!(!crate::store::bots::stage_closed(&c, room)?, "The stage is closed");
        let context = member(&c, room, bot)?.1;
        let waiting = blocked(&c, room, bot, &context)?;
        Ok((context, waiting))
    }

    /// 新会话里的原任务放不下时，允许缩短待使用摘要。
    pub(crate) fn review_shorter_member_summary(&self, bot: &str, room: &str) -> Result<Option<Value>> {
        let c = self.db.locked();
        let _ = member(&c, room, bot)?;
        Ok(read_request(&c, &request_key(bot, Some(room)))?
            .filter(|r| r.needs_shorter_summary && r.handoff.phase == Phase::Switched)
            .map(|r| json!({"text":r.handoff.text,"revision":r.handoff.revision})))
    }

    /// 预算预检失败留一个可编辑入口，排队任务保持原身份。
    pub(crate) fn request_shorter_member_summary(&self, bot: &str, room: &str, revision: &str) -> Result<()> {
        self.db.locked().execute(
            "UPDATE kv SET v=json_set(v,'$.needs_shorter_summary',json('true'))
            WHERE k=?1 AND json_extract(v,'$.handoff.revision')=?2 AND json_extract(v,'$.handoff.phase')='switched'",
            params![request_key(bot, Some(room)), revision],
        )?;
        Ok(())
    }

    /// 生成摘要后检查会话身份，普通群只把私聊卡标识返回给群。
    pub(crate) fn prepare_member_session(
        &self,
        bot: &str,
        draft: &MemberSessionRequest,
    ) -> Result<(Value, Vec<Entry>)> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = member(&tx, &draft.room, bot)?.1;
        if let Some(waiting) = blocked(&tx, &draft.room, bot, &active)? {
            return Ok((waiting, Vec::new()));
        }
        ensure!(same_session(draft, &active) && active.recommend(), "The session has changed. Open the review again.");
        let key = request_key(bot, (!active.shared).then_some(&draft.room));
        let mut cards = Vec::new();
        let mut r = draft.clone();
        if let Some(mut old) = read_request(&tx, &key)? {
            if same_session(&old, &active) && old.handoff.phase == Phase::Approved {
                return Ok((json!({"phase":"approved"}), cards));
            }
            if same_session(&old, &active) && old.room == draft.room && old.handoff.phase == Phase::Draft {
                r = old;
            } else if old.handoff.phase == Phase::Draft {
                old.handoff.phase = Phase::Superseded;
                cards.extend(save(&tx, bot, &key, &old)?);
            }
        }
        cards.extend(save(&tx, bot, &key, &r)?);
        tx.commit()?;
        let preview = if active.shared {
            json!({"privateEntryId":private_id(&r)})
        } else {
            json!({"text":r.handoff.text,"revision":r.handoff.revision})
        };
        Ok((preview, cards))
    }

    /// 验证来源、最新草稿和会话身份，确认后原子持久化等待中的操作。
    pub(crate) fn decide_member_session(
        &self,
        room: &str,
        bot: &str,
        shared: bool,
        revision: &str,
        confirm: bool,
        text: Option<&str>,
    ) -> Result<Vec<Entry>> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(!crate::store::bots::stage_closed(&tx, room)?, "The stage is closed");
        let active = member(&tx, room, bot)?.1;
        ensure!(active.shared == shared, "Review the handoff in its own chat");
        let key = request_key(bot, (!shared).then_some(room));
        let mut r = read_request(&tx, &key)?.ok_or_else(|| anyhow!("Handoff no longer exists"))?;
        ensure!(r.room == room && r.handoff.revision == revision, "The handoff has changed. Open the review again.");
        if !shared && confirm && r.needs_shorter_summary && r.handoff.phase == Phase::Switched {
            let text = text.unwrap_or_default().trim();
            ensure!(!text.is_empty() && text.chars().count() <= 20_000, "Use 1 to 20,000 characters for the summary.");
            text.clone_into(&mut r.handoff.text);
            r.needs_shorter_summary = false;
            let cards = save(&tx, bot, &key, &r)?;
            tx.commit()?;
            return Ok(cards);
        }
        if confirm && matches!(r.handoff.phase, Phase::Approved | Phase::Switched | Phase::Sent) {
            return Ok(Vec::new());
        }
        ensure!(
            r.handoff.phase == Phase::Draft && same_session(&r, &active),
            "The session has changed. Open the review again."
        );
        if let Some(text) = text {
            let text = text.trim();
            ensure!(!text.is_empty() && text.chars().count() <= 20_000, "Use 1 to 20,000 characters for the summary.");
            text.clone_into(&mut r.handoff.text);
        }
        if confirm {
            ensure!(
                blocked(&tx, room, bot, &active)?.is_none(),
                "An assignment needs its handoff. Open the member card again."
            );
            r.handoff.phase = Phase::Approved;
        }
        let cards = save(&tx, bot, &key, &r)?;
        tx.commit()?;
        Ok(cards)
    }

    /// 启动和回合结束恢复该成员已确认的请求。
    pub(crate) fn queued_member_sessions(&self, bot: &str) -> Result<Vec<MemberSessionRequest>> {
        let c = self.db.locked();
        let prefix = format!("group.member-session.{bot}.");
        let upper = format!("group.member-session.{bot}/");
        let mut statement = c.prepare(
            "SELECT v FROM kv WHERE k>=?1 AND k<?2 AND
             (json_extract(v,'$.handoff.phase')='approved' OR json_extract(v,'$.retirement_pending')=1)",
        )?;
        let rows = statement.query_map([prefix, upper], |r| r.get::<_, String>(0))?;
        rows.map(|v| Ok(serde_json::from_str(&v?)?)).collect()
    }

    /// 在空闲边界原子归档。只有原会话仍归属该请求时才摘除活跃槽。
    pub(crate) fn apply_member_session(&self, bot: &str, r: &MemberSessionRequest) -> Result<(bool, Vec<Entry>)> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = request_key(bot, r.handoff.slot.as_ref().map(|_| r.room.as_str()));
        let Some(mut current) = read_request(&tx, &key)? else { return Ok((false, Vec::new())) };
        if current.handoff.phase != Phase::Approved || current.handoff.revision != r.handoff.revision {
            return Ok((false, Vec::new()));
        }
        let applied = if let Ok((_, ctx)) = member(&tx, &r.room, bot) {
            same_session(r, &ctx)
                && !crate::store::bots::stage_closed(&tx, &r.room)?
                && blocked(&tx, &r.room, bot, &ctx)?.is_none()
        } else {
            false
        };
        if applied {
            archive_context(&tx, bot, &r.room, &r.handoff.revision, &r.handoff)?;
            if r.handoff.slot.is_none() {
                tx.execute(
                    "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                    params![private_key(bot), r.handoff.text],
                )?;
            }
            current.handoff.phase = Phase::Switched;
            current.retirement_pending = true;
        } else {
            current.handoff.phase = Phase::Superseded;
        }
        let cards = save(&tx, bot, &key, &current)?;
        tx.commit()?;
        Ok((applied, cards))
    }

    /// 记录执行器端归档结果，重启后可恢复到同一支持持久 ID 的执行器。
    pub(crate) fn finish_member_retirement(&self, bot: &str, request: &MemberSessionRequest) -> Result<()> {
        self.db.locked().execute(
            "UPDATE kv SET v=json_set(v,'$.retirement_pending',json('false'))
            WHERE k=?1 AND json_extract(v,'$.handoff.revision')=?2",
            params![
                request_key(bot, request.handoff.slot.as_ref().map(|_| request.room.as_str())),
                request.handoff.revision
            ],
        )?;
        Ok(())
    }

    /// 独立群的交接只给该群下一条业务派工，等待成功写入执行器再消费。
    pub(crate) fn member_group_summary(&self, bot: &str, room: &str) -> Result<Option<GroupHandoff>> {
        Ok(read_request(&self.db.locked(), &request_key(bot, Some(room)))?
            .filter(|r| r.handoff.phase == Phase::Switched)
            .map(|r| r.handoff))
    }

    /// 只消费实际已使用的交接，避免覆写后来创建的草稿。
    pub(crate) fn consume_member_group_summary(&self, bot: &str, room: &str, revision: &str) -> Result<()> {
        self.db.locked().execute(
            "UPDATE kv SET v=json_set(v,'$.handoff.phase','sent')
            WHERE k=?1 AND json_extract(v,'$.handoff.revision')=?2 AND json_extract(v,'$.handoff.phase')='switched'",
            params![request_key(bot, Some(room)), revision],
        )?;
        Ok(())
    }
}
