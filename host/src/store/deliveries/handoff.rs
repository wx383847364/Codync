//! 群派工的交接、会话归档和原卡更新在同一事务提交。

use super::{Delivery, Status, completion, eligible, get};
use crate::{
    LockExt,
    store::{Entry, EntryKind, Store, now_ms, run_session_key},
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    Draft,
    Approved,
    Switched,
    Sent,
    Superseded,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct GroupHandoff {
    pub phase: Phase,
    pub revision: String,
    pub session: String,
    pub generation: Option<String>,
    pub detached: bool,
    pub replacement: Option<String>,
    pub replacement_generation: Option<String>,
    pub backend: String,
    pub model: Option<String>,
    pub slot: Option<String>,
    pub text: String,
    pub window: u64,
    pub incoming: u64,
}

/// 每条原派工只有一份可变交接，标识与消息内容无关。
fn key(id: &str) -> String {
    format!("group.handoff.{id}")
}

/// 在事务中读取当前交接，供投影和会话切换验证同一状态。
fn read(c: &Connection, id: &str) -> Result<Option<GroupHandoff>> {
    let value: Option<String> = c.query_row("SELECT v FROM kv WHERE k=?", [key(id)], |r| r.get(0)).optional()?;
    Ok(value.map(|v| serde_json::from_str(&v)).transpose()?)
}

/// 群只收到会话作用域和派工状态，私聊交接正文从不经过原派工卡。
pub(super) fn projection(c: &Connection, d: &Delivery) -> Result<Value> {
    let Some(handoff) = read(c, &d.id)? else { return Ok(Value::Null) };
    let waiting =
        d.status == Status::Queued && matches!(handoff.phase, Phase::Draft | Phase::Approved | Phase::Switched);
    if !waiting && handoff.phase != Phase::Sent {
        return Ok(Value::Null);
    }
    let private_entry = handoff.slot.is_none().then(|| private_card_id(d, &handoff));
    Ok(json!({"phase":handoff.phase,"shared":handoff.slot.is_none(),"privateEntryId":private_entry}))
}

/// 写交接和投影，主会话的编辑卡始终位于机器人私聊。
fn save(c: &Connection, d: &Delivery, handoff: &GroupHandoff) -> Result<Vec<Entry>> {
    c.execute(
        "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![key(&d.id), serde_json::to_string(handoff)?],
    )?;
    let mut cards = vec![completion::card(c, d)?];
    cards.extend(private_card(c, d, handoff)?);
    Ok(cards)
}

/// 给共享群只提供定位用的公开卡片标识，内容始终由私聊读取。
fn private_card_id(d: &Delivery, handoff: &GroupHandoff) -> String {
    format!("group-handoff-{}-{}", d.id, handoff.revision)
}

/// 原群取消或删除时也能单独撤下仍存在的私聊草稿。
fn private_card(c: &Connection, d: &Delivery, handoff: &GroupHandoff) -> Result<Option<Entry>> {
    if handoff.slot.is_none()
        && c.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=? AND deleted=0)", [&d.target_bot_id], |r| {
            r.get::<_, bool>(0)
        })?
    {
        let (status, line) = match handoff.phase {
            Phase::Draft => ("draft", "群派工等待确认新开会话。确认后也会为这个机器人的私聊新开会话。"),
            Phase::Approved | Phase::Switched => ("approved", "已确认，正在准备新会话。"),
            Phase::Sent => ("sent", "已开启新会话，原群派工继续执行。摘要会带入下一条私聊消息。"),
            Phase::Superseded => ("kept", "这条派工的交接已失效。"),
        };
        let turn =
            c.query_row("SELECT COALESCE(MAX(turn),0) FROM entries WHERE bot_id=?", [&d.target_bot_id], |r| r.get(0))?;
        let name: String = c
            .query_row("SELECT COALESCE(json_extract(config,'$.name'),id) FROM bots WHERE id=?", [&d.room_id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_else(|| d.room_id.clone());
        // 同一机器人可以同时收到多个群派工，在私聊中保留来源和短预览以便辨认。
        let source = d
            .source_entry_id
            .as_ref()
            .map(|entry| {
                c.query_row("SELECT json_extract(data,'$.text') FROM entries WHERE id=?", [entry], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .optional()
            })
            .transpose()?
            .flatten()
            .flatten();
        let preview = d.body.as_deref().or(source.as_deref()).unwrap_or_default();
        let preview: String = preview.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
        let private_line = if preview.is_empty() {
            format!("{name} · {line}")
        } else {
            format!("{name} · {line}\n原派工：{preview}")
        };
        return Ok(Some(completion::entry(
            c,
            &private_card_id(d, handoff),
            &d.target_bot_id,
            EntryKind::Notice,
            turn,
            &json!({"text":private_line,"style":"info","groupDeliveryId":d.id,
                "handoff":{"status": status, "reason":"groupCapacity", "text":handoff.text, "revision":handoff.revision}}),
        )?));
    }
    Ok(None)
}

/// 待派工仍有效才允许用户修改或确认，取消与退群不会因交接而恢复。
fn queued(c: &Connection, id: &str) -> Result<Delivery> {
    let d = get(c, id)?.ok_or_else(|| anyhow!("找不到原派工"))?;
    ensure!(
        d.status == Status::Queued
            && eligible(c, &d.room_id, &d.target_bot_id, d.source_bot_id.as_deref())?
            && !super::super::bots::stage_closed(c, &d.room_id)?,
        "原派工已结束或不可继续"
    );
    Ok(d)
}

/// 校验摘要长度及窗口，给任务原文和模型回复预留空间。
fn checked_text(text: &str, current: &GroupHandoff) -> Result<String> {
    let text = text.trim();
    ensure!(!text.is_empty() && text.chars().count() <= 20_000, "摘要需要 1 至 20,000 个字符");
    let extra = codync_mem::tokens::estimate(text) as u64;
    let reserve = (current.window / 16).clamp(2_048, 8_192);
    ensure!(
        current.slot.is_none() || current.incoming.saturating_add(extra).saturating_add(reserve) <= current.window,
        "这份摘要和原派工超出会话窗口，请缩短摘要"
    );
    Ok(text.into())
}

impl Store {
    /// 读取持久化交接，进程重启后仍可恢复确认中的原任务。
    pub(crate) fn group_handoff(&self, id: &str) -> Result<Option<GroupHandoff>> {
        read(&self.db.locked(), id)
    }

    /// 创建待确认交接只在原派工尚未领取时生效。
    pub(crate) fn draft_group_handoff(&self, id: &str, draft: &GroupHandoff) -> Result<Vec<Entry>> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let d = queued(&tx, id)?;
        if read(&tx, id)?.is_some_and(|h| h.phase != Phase::Superseded) {
            return Ok(vec![]);
        }
        // 旧客户端及派工诊断仍能从原状态行识别上下文不足。
        let detail = "上下文空间不足，等待确认新开会话。也可在成员设置中选择更大窗口的模型。";
        tx.execute("UPDATE deliveries SET last_error=?2 WHERE id=?1", params![id, detail])?;
        let cards = save(&tx, &d, draft)?;
        tx.commit()?;
        Ok(cards)
    }

    /// 摘要修改和确认原子完成，重复确认无法改写已接受的摘要。
    pub(crate) fn decide_group_handoff(
        &self,
        id: &str,
        shared: bool,
        confirm: bool,
        revision: &str,
        text: Option<&str>,
    ) -> Result<Vec<Entry>> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut handoff = read(&tx, id)?.ok_or_else(|| anyhow!("没有待确认的交接"))?;
        ensure!(handoff.revision == revision, "交接已更新，请重新打开后确认");
        ensure!(handoff.slot.is_none() == shared, "请在交接所属聊天里操作");
        if confirm && matches!(handoff.phase, Phase::Approved | Phase::Switched | Phase::Sent) {
            return Ok(vec![]);
        }
        let d = queued(&tx, id)?;
        ensure!(handoff.phase == Phase::Draft, "这份交接已失效");
        if let Some(text) = text {
            handoff.text = checked_text(text, &handoff)?;
        }
        if confirm {
            checked_text(&handoff.text, &handoff)?;
            handoff.phase = Phase::Approved;
        }
        let cards = save(&tx, &d, &handoff)?;
        tx.commit()?;
        Ok(cards)
    }

    /// 确认后只在匹配的旧会话上执行切换，归档与摘除活跃引用同事务提交。
    pub(crate) fn switch_group_handoff(
        &self,
        id: &str,
        backend: &str,
        model: Option<&str>,
        slot: Option<&str>,
        incoming: Option<u64>,
    ) -> Result<(Option<GroupHandoff>, Vec<Entry>)> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let d = queued(&tx, id)?;
        let Some(mut handoff) = read(&tx, id)? else { return Ok((None, vec![])) };
        if !matches!(handoff.phase, Phase::Draft | Phase::Approved | Phase::Switched) {
            return Ok((Some(handoff), vec![]));
        }

        let active = active_session(&tx, &d.target_bot_id, slot)?;
        let generation = active
            .as_deref()
            .map(|sid| super::member_session::scoped_generation(&tx, &d.target_bot_id, backend, slot, sid))
            .transpose()?
            .flatten();
        let original_matches = if handoff.detached {
            // 只接续本派工在准备阶段登记的新会话，拒绝重启后被别的工作占用的槽。
            active.is_none() || (active == handoff.replacement && generation == handoff.replacement_generation)
        } else {
            active.as_deref() == Some(&handoff.session) && generation == handoff.generation
        };
        if handoff.backend != backend
            || handoff.model.as_deref() != model
            || handoff.slot.as_deref() != slot
            || !original_matches
        {
            handoff.phase = Phase::Superseded;
        } else if let Some(incoming) = incoming.filter(|_| matches!(handoff.phase, Phase::Approved | Phase::Switched)) {
            handoff.incoming = incoming;
            let extra = slot.map_or(0, |_| codync_mem::tokens::estimate(&handoff.text) as u64);
            let reserve = (handoff.window / 16).clamp(2_048, 8_192);
            if incoming.saturating_add(extra).saturating_add(reserve) > handoff.window {
                // 等待期间材料可能变大；保留旧会话并允许再次缩短交接。
                handoff.phase = Phase::Draft;
                tx.execute(
                    "UPDATE deliveries SET last_error=?2 WHERE id=?1",
                    params![id, "任务材料已更新，新会话放不下。请缩短摘要或为机器人选择更大窗口的模型。"],
                )?;
                let cards = save(&tx, &d, &handoff)?;
                tx.commit()?;
                return Ok((Some(handoff), cards));
            }
            if handoff.phase == Phase::Switched {
                return Ok((Some(handoff), vec![]));
            }
            if !handoff.detached {
                archive_context(&tx, &d.target_bot_id, &d.room_id, id, &handoff)?;
                handoff.detached = true;
            }
            if handoff.slot.is_none() {
                // 共享群切换后，私聊摘要始终等待下一条私聊消息。
                tx.execute(
                    "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                    params![private_key(&d.target_bot_id), private_value(&d.room_id, &handoff.text)],
                )?;
            }
            handoff.phase = Phase::Switched;
        } else {
            return Ok((Some(handoff), vec![]));
        }
        let cards = save(&tx, &d, &handoff)?;
        tx.commit()?;
        Ok((Some(handoff), cards))
    }

    /// 写前登记原派工准备的新执行器会话，随后才允许活跃槽指向它。
    pub(crate) fn register_group_replacement(
        &self,
        id: &str,
        backend: &str,
        slot: Option<&str>,
        sid: &str,
    ) -> Result<()> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(mut handoff) = read(&tx, id)?
            && handoff.phase == Phase::Switched
            && handoff.backend == backend
            && handoff.slot.as_deref() == slot
        {
            handoff.replacement = Some(sid.to_owned());
            let bot = get(&tx, id)?.ok_or_else(|| anyhow!("Delivery no longer exists"))?.target_bot_id;
            handoff.replacement_generation = super::member_session::scoped_generation(&tx, &bot, backend, slot, sid)?;
            tx.execute("UPDATE kv SET v=?2 WHERE k=?1", params![key(id), serde_json::to_string(&handoff)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 只有执行器已收到原派工的 prompt，才能显示交接已接续。
    pub(crate) fn group_handoff_sent(&self, id: &str) -> Result<Vec<Entry>> {
        let mut db = self.db.locked();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(d) = get(&tx, id)? else { return Ok(vec![]) };
        let Some(mut handoff) = read(&tx, id)? else { return Ok(vec![]) };
        if d.status != Status::Started || handoff.phase != Phase::Switched {
            return Ok(vec![]);
        }
        handoff.phase = Phase::Sent;
        let cards = save(&tx, &d, &handoff)?;
        tx.commit()?;
        Ok(cards)
    }
}

/// 终态和私聊卡在同一事务提交，重启或删除原群也不会留下可确认草稿。
pub(super) fn discard(c: &Connection, d: &Delivery) -> Result<Option<Entry>> {
    let Some(mut handoff) = read(c, &d.id)? else { return Ok(None) };
    if !matches!(handoff.phase, Phase::Draft | Phase::Approved | Phase::Switched) {
        return Ok(None);
    }
    handoff.phase = Phase::Superseded;
    c.execute("UPDATE kv SET v=?2 WHERE k=?1", params![key(&d.id), serde_json::to_string(&handoff)?])?;
    private_card(c, d, &handoff)
}

/// 普通群的私聊摘要只供下一条私聊用户消息读取。
pub(crate) fn private_key(bot: &str) -> String {
    format!("group.handoff.private.{bot}")
}

/// 保存源群，用于清理只属于该群的私聊交接提示。
pub(super) fn private_value(room: &str, text: &str) -> String {
    json!({"room":room,"text":text}).to_string()
}

/// 新记录包含源群，旧版已经保存的纯文本仍能带入下一条私聊。
pub(crate) fn private_text(value: String) -> String {
    if let Ok(record) = serde_json::from_str::<Value>(&value)
        && record["room"].is_string()
        && let Some(text) = record["text"].as_str()
    {
        return text.to_owned();
    }
    value
}

/// 在同一事务记录旧会话和停泊会话，再移除活跃引用。
pub(super) fn archive_context(c: &Connection, bot: &str, room: &str, id: &str, handoff: &GroupHandoff) -> Result<()> {
    let parked_prefix = if handoff.slot.is_some() {
        run_session_key(bot, &format!("group/{room}/"))
    } else {
        format!("fallback.session.{bot}.")
    };
    // 一个逻辑会话可能有多个执行器的停泊记录，归档范围限定为同一群或共享主会话。
    let parked = c
        .prepare("SELECT k,v FROM kv WHERE substr(k,1,length(?1))=?1 ORDER BY k")?
        .query_map([&parked_prefix], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let chat = if handoff.slot.is_some() { room } else { bot };
    // 原派工可能在切换前就已排队，其用户消息属于新执行。
    let before = if handoff.slot.is_some() {
        get(c, id)?
            .and_then(|delivery| delivery.source_entry_id)
            .map(|source| {
                c.query_row("SELECT seq FROM entries WHERE id=? AND bot_id=?", params![source, chat], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    let anchor = archive_anchor(c, chat, before)?;
    let archive = format!("session.archive.{bot}.{id}");
    c.execute(
        "INSERT INTO kv(k,v) VALUES(?1,?2)",
        params![
            archive,
            json!({
                "sessionId":handoff.session, "backend":handoff.backend, "roomId":room,
                "slot":handoff.slot,"parked":parked,"archivedAt":now_ms(),"anchor":anchor,"state":"retained"
            })
            .to_string()
        ],
    )?;
    c.execute("DELETE FROM kv WHERE substr(k,1,length(?1))=?1", [parked_prefix])?;
    if handoff.slot.is_none() {
        c.execute("UPDATE bots SET session_id=NULL WHERE id=?", [bot])?;
        let seq: i64 = c.query_row("SELECT COALESCE(MAX(seq),0) FROM entries WHERE bot_id=?", [bot], |r| r.get(0))?;
        c.execute(
            "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![format!("fallback.fresh.{}", bot), seq.to_string()],
        )?;
    }
    Ok(())
}

/// 只定位到切换前已存在的可见聊天消息。
pub(crate) fn archive_anchor(c: &Connection, chat: &str, before: Option<i64>) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT MAX(seq) FROM entries WHERE bot_id=?1 AND (?2 IS NULL OR seq<?2) AND kind IN ('user','agent')
         AND json_extract(data,'$.removed') IS NOT 1 AND ((kind='user' AND COALESCE(json_extract(data,'$.status'),'sent')='sent') OR (kind='agent' AND json_extract(data,'$.final') IS 1))",
        params![chat, before],
        |r| r.get(0),
    )?)
}

/// 确认、恢复和用量投影读取同一个已保存的会话槽。
pub(super) fn active_session(c: &Connection, bot: &str, slot: Option<&str>) -> Result<Option<String>> {
    Ok(if let Some(slot) = slot {
        c.query_row("SELECT v FROM kv WHERE k=?", [run_session_key(bot, slot)], |r| r.get(0)).optional()?
    } else {
        c.query_row("SELECT session_id FROM bots WHERE id=? AND deleted=0", [bot], |r| r.get(0)).optional()?.flatten()
    })
}

impl Store {
    /// 记录执行器返回的确定状态，进程标识只用于 Host 的删除归属检查。
    pub(crate) fn mark_session_history(&self, bot: &str, id: &str, state: &str, opened: Option<&str>) -> Result<()> {
        let changed = self.db.locked().execute(
            "UPDATE kv SET v=json_set(v,'$.state',?2,'$.opened',?3) WHERE k=?1",
            params![format!("session.archive.{bot}.{id}"), state, opened],
        )?;
        ensure!(changed == 1, "Session history no longer exists");
        Ok(())
    }

    /// 主会话和独立群各自查询，普通群读主会话时必须主动导航到 Bot 私聊。
    pub(crate) fn session_history(
        &self,
        bot: &str,
        room: Option<&str>,
        id: Option<&str>,
    ) -> Result<(bool, Vec<(String, Value)>)> {
        let c = self.db.locked();
        let has_main: bool = c.query_row(
            "SELECT session_id IS NOT NULL FROM bots WHERE id=? AND deleted=0 AND COALESCE(json_extract(config,'$.kind'),'bot') <> 'group'",
            [bot], |r| r.get(0),
        ).optional()?.unwrap_or(false);
        let active = if let Some(room) = room {
            let (_, context) = super::member_session::member(&c, room, bot)?;
            ensure!(!context.shared, "Open the bot’s private chat for its main session history.");
            context.session.is_some()
        } else {
            has_main
        };
        let prefix = format!("session.archive.{bot}.");
        let mut query = c.prepare(
            "SELECT substr(k,length(?1)+1),v FROM kv WHERE substr(k,1,length(?1))=?1
             AND CASE WHEN ?2 IS NULL THEN json_extract(v,'$.slot') IS NULL
                 ELSE json_extract(v,'$.slot') IS NOT NULL AND json_extract(v,'$.roomId')=?2 END
             AND (?3 IS NULL OR k=?1 || ?3)
             ORDER BY json_extract(v,'$.archivedAt') DESC,k DESC LIMIT 100",
        )?;
        let rows =
            query.query_map(params![prefix, room, id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, raw) = row?;
            entries.push((id, serde_json::from_str(&raw)?));
        }
        Ok((active, entries))
    }

    /// 删除前拒绝仍处于主会话、运行槽或停泊引用中的 ID。
    pub(crate) fn session_is_active(&self, bot: &str, sid: &str) -> Result<bool> {
        let c = self.db.locked();
        let fallback = format!("fallback.session.{bot}.");
        let runs = run_session_key(bot, "");
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND session_id=?2 AND deleted=0)
             OR EXISTS(SELECT 1 FROM kv WHERE
                 (substr(k,1,length(?3))=?3 AND (v=?2 OR json_extract(CASE WHEN json_valid(v) THEN v ELSE '{}' END,'$.session')=?2))
                 OR (substr(k,1,length(?4))=?4 AND v=?2))",
            params![bot, sid, fallback, runs], |r| r.get(0),
        )?)
    }

    /// 按已验证历史条目的锚点读取旧聊天，仅返回用户与最终回复的文本。
    pub(crate) fn old_session_messages(&self, bot: &str, room: Option<&str>, id: &str) -> Result<Value> {
        let (_, entries) = self.session_history(bot, room, Some(id))?;
        let (_, item) = entries.first().ok_or_else(|| anyhow!("Session history no longer exists"))?;
        let Some(anchor) = item["anchor"].as_i64() else { return Ok(json!({"messages":[]})) };
        let c = self.db.locked();
        let mut query = c.prepare(
            "SELECT id,kind,created_at,json_extract(data,'$.author'),json_extract(data,'$.text') FROM entries
             WHERE bot_id=?1 AND seq<=?2 AND json_extract(data,'$.removed') IS NOT 1
             AND ((kind='user' AND COALESCE(json_extract(data,'$.status'),'sent')='sent') OR (kind='agent' AND json_extract(data,'$.final') IS 1))
             AND json_type(data,'$.text')='text' ORDER BY seq DESC LIMIT 25",
        )?;
        let mut messages = query
            .query_map(params![room.unwrap_or(bot), anchor], |r| {
                Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"at":r.get::<_,i64>(2)?,
                "author":r.get::<_,Option<String>>(3)?,"text":r.get::<_,String>(4)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(json!({"messages":messages}))
    }
}
