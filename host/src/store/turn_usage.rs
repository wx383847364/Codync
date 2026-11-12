//! 已观察到的 ACP 执行尝试，使用现有本地数据库保留重启和日志轮转前的计数。

use super::{Store, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

/// 实报和汇总共用字段定义，各供应方的字段原样呈现。
pub(crate) const TOKEN_FIELDS: [(&str, &str); 5] = [
    ("inputTokens", "Input tokens"),
    ("cachedReadTokens", "Cache read tokens"),
    ("cachedWriteTokens", "Cache write tokens"),
    ("outputTokens", "Output tokens"),
    ("thoughtTokens", "Thought tokens"),
];

pub(crate) struct AttemptStart<'a> {
    pub id: &'a str,
    pub bot: &'a str,
    pub room: Option<&'a str>,
    pub delivery: Option<&'a str>,
    pub backend: &'a str,
    pub source: &'a str,
    pub requests: &'a [&'a str],
}

#[derive(Clone, Copy)]
pub(crate) enum UsageEvent {
    Prompt,
    Session,
    Process,
}

/// 独立的尝试主键允许回退和重试分别计数。观测开始前的记录不从旧投递猜测。
pub(super) fn schema(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS turn_usage_attempts (
            id TEXT PRIMARY KEY, bot_id TEXT NOT NULL, room_id TEXT, delivery_id TEXT,
            backend TEXT NOT NULL, model TEXT, source TEXT NOT NULL, request_ids TEXT NOT NULL,
            started_at INTEGER NOT NULL, finished_at INTEGER, duration_ms INTEGER,
            outcome TEXT NOT NULL DEFAULT 'running', prompts INTEGER NOT NULL DEFAULT 0,
            sessions INTEGER NOT NULL DEFAULT 0, processes INTEGER NOT NULL DEFAULT 0, reported TEXT
        );
        CREATE INDEX IF NOT EXISTS turn_usage_room ON turn_usage_attempts(room_id, backend, model, source);
        CREATE INDEX IF NOT EXISTS turn_usage_delivery ON turn_usage_attempts(delivery_id) WHERE delivery_id IS NOT NULL;",
    )?;
    c.execute("INSERT OR IGNORE INTO kv(k,v) VALUES('turn.usage.since',?)", [now_ms()])?;
    Ok(())
}

impl Store {
    /// 执行器初始化之前固定尝试身份；不存提示、工具内容或会话正文。
    pub(crate) fn begin_usage_attempt(&self, attempt: &AttemptStart<'_>) -> Result<()> {
        self.db.locked().execute(
            "INSERT OR IGNORE INTO turn_usage_attempts(id,bot_id,room_id,delivery_id,backend,source,request_ids,started_at)
             VALUES(?,?,?,?,?,?,?,?)",
            params![attempt.id, attempt.bot, attempt.room, attempt.delivery, attempt.backend,
                attempt.source, serde_json::to_string(attempt.requests)?, now_ms()],
        )?;
        Ok(())
    }

    /// 计数只来自实际观察到的协议或进程事件；默认模型未确认时维持 NULL。
    pub(crate) fn observe_usage_event(&self, id: &str, event: UsageEvent, model: Option<&str>) -> Result<()> {
        let db = self.db.locked();
        match event {
            UsageEvent::Prompt => db.execute(
                "UPDATE turn_usage_attempts SET prompts=prompts+1,model=?2 WHERE id=?1 AND finished_at IS NULL",
                params![id, model],
            )?,
            UsageEvent::Session => db.execute(
                "UPDATE turn_usage_attempts SET sessions=sessions+1 WHERE id=? AND finished_at IS NULL",
                [id],
            )?,
            UsageEvent::Process => db.execute(
                "UPDATE turn_usage_attempts SET processes=processes+1 WHERE id=? AND finished_at IS NULL",
                [id],
            )?,
        };
        Ok(())
    }

    /// 第一次完成记录终态和最终实报，重复响应或迟到完成不能重复统计。
    pub(crate) fn finish_usage_attempt(&self, id: &str, outcome: &str, elapsed: i64, report: &Value) -> Result<()> {
        let known = (report["source"] == "provider_reported").then(|| report.to_string());
        self.db.locked().execute(
            "UPDATE turn_usage_attempts SET finished_at=?2,outcome=?3,duration_ms=?4,reported=?5 WHERE id=?1 AND finished_at IS NULL",
            params![id, now_ms(), outcome, elapsed, known],
        )?;
        Ok(())
    }

    /// Host 退出时未能收束的尝试保持缺报，重启不重发也不推断消耗。
    pub(crate) fn interrupt_usage_attempts(&self) -> Result<()> {
        self.db.locked().execute(
            "UPDATE turn_usage_attempts SET finished_at=?1,outcome='interrupted' WHERE finished_at IS NULL",
            [now_ms()],
        )?;
        Ok(())
    }

    /// 每个阶段只汇总自己的尝试，缺报、历史缺口及无法归属的尝试分别给出。
    pub(crate) fn stage_usage(&self, room: &str) -> Result<Value> {
        let c = self.db.locked();
        let since: i64 =
            c.query_row("SELECT CAST(v AS INTEGER) FROM kv WHERE k='turn.usage.since'", [], |r| r.get(0))?;
        let (deliveries, retries, unmatched): (i64, i64, i64) = c.query_row(
            "SELECT COUNT(*), COALESCE(SUM(parent_id IS NOT NULL),0), COALESCE(SUM(started_at IS NOT NULL AND NOT EXISTS(
                SELECT 1 FROM turn_usage_attempts t WHERE t.delivery_id=d.id)),0)
             FROM deliveries d WHERE room_id=?",
            [room],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let counts = c.query_row(
            "SELECT COUNT(*),COALESCE(SUM(prompts),0),COALESCE(SUM(sessions),0),
                COALESCE(SUM(processes),0),COALESCE(SUM(reported IS NULL),0),SUM(duration_ms)
             FROM turn_usage_attempts WHERE room_id=?",
            [room],
            |r| {
                Ok(json!({"attempts":r.get::<_, i64>(0)?, "prompts":r.get::<_, i64>(1)?,
                "sessions":r.get::<_, i64>(2)?, "processes":r.get::<_, i64>(3)?,
                "missingReports":r.get::<_, i64>(4)?, "durationMs":r.get::<_, Option<i64>>(5)?}))
            },
        )?;
        let outcomes = counts_by(&c, room, "outcome")?;
        let triggers = counts_by(&c, room, "source")?;
        let unassigned = c
            .prepare("SELECT source,COUNT(*) FROM turn_usage_attempts WHERE room_id IS NULL GROUP BY source")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, Value::from(r.get::<_, i64>(1)?))))?
            .collect::<rusqlite::Result<Map<_, _>>>()?;
        let providers = provider_reports(&c, room)?;
        drop(c);
        let mut summary = json!({"observedSince":since,"deliveries":deliveries,"retries":retries,"startedDeliveriesWithoutAttempt":unmatched,
            "counts":counts,"outcomes":outcomes,"triggers":triggers,"providers":providers,"unassigned":unassigned});
        summary["sections"] = display_sections(&summary);
        Ok(summary)
    }
}

/// 各端共用指标顺序和口径，标题用界面字典翻译，模型标识保持原样。
fn display_sections(usage: &Value) -> Value {
    let counts = &usage["counts"];
    let outcomes = &usage["outcomes"];
    let row = |label: &str, value: &Value| json!({"label":label,"value":value});
    let zero = json!(0);
    let mut sections = vec![json!({"label":"Stage usage","rows":[
        row("Deliveries", &usage["deliveries"]), row("Retry deliveries", &usage["retries"]), row("Host attempts", &counts["attempts"]),
        row("ACP prompts", &counts["prompts"]), row("New sessions", &counts["sessions"]),
        row("Agent processes", &counts["processes"]), row("Attempts without token reports", &counts["missingReports"]),
        row("Started deliveries without an observed attempt", &usage["startedDeliveriesWithoutAttempt"]),
        row("Observed duration (ms)", &counts["durationMs"]),
        row("No reply", outcomes.get("noReply").unwrap_or(&zero)),
        row("Completed", outcomes.get("completed").unwrap_or(&zero)),
        row("Incomplete", outcomes.get("incomplete").unwrap_or(&zero)),
        row("Failed", outcomes.get("failed").unwrap_or(&zero)),
        row("Cancelled", outcomes.get("cancelled").unwrap_or(&zero)),
        row("Interrupted", outcomes.get("interrupted").unwrap_or(&zero)),
        row("In progress", outcomes.get("running").unwrap_or(&zero)),
        row("Context maintenance", usage["triggers"].get("maintenance").unwrap_or(&zero)),
    ]})];
    for provider in usage["providers"].as_array().into_iter().flatten() {
        let rows = TOKEN_FIELDS
            .into_iter()
            .map(|(field, label)| {
                json!({"label":label,"value":provider["tokens"][field],
            "reports":provider["reported"][field],"attempts":provider["attempts"]})
            })
            .collect::<Vec<_>>();
        sections.push(
            json!({"label":"Reported tokens", "backend":provider["backend"], "model":provider["model"], "rows":rows}),
        );
    }
    let unassigned = &usage["unassigned"];
    sections.push(json!({"label":"Unassigned on this host","rows":[
        row("Private chats", unassigned.get("private").unwrap_or(&zero)),
        row("Routines", unassigned.get("routine").unwrap_or(&zero)),
        row("Shared coordination", unassigned.get("shared_coordinator").unwrap_or(&zero)),
        row("Direct bot requests", unassigned.get("request_execution").unwrap_or(&zero)),
        row("Host work", unassigned.get("host").unwrap_or(&zero)),
        row("Context maintenance", unassigned.get("maintenance").unwrap_or(&zero)),
    ]}));
    Value::Array(sections)
}

/// 用固定列名查询触发类型或终态计数，调用方不会把用户输入拼进 SQL。
fn counts_by(c: &Connection, room: &str, column: &str) -> Result<Map<String, Value>> {
    let sql = if column == "outcome" {
        "SELECT outcome,COUNT(*) FROM turn_usage_attempts WHERE room_id=? GROUP BY outcome"
    } else {
        "SELECT source,COUNT(*) FROM turn_usage_attempts WHERE room_id=? GROUP BY source"
    };
    Ok(c.prepare(sql)?
        .query_map([room], |r| Ok((r.get::<_, String>(0)?, Value::from(r.get::<_, i64>(1)?))))?
        .collect::<rusqlite::Result<_>>()?)
}

/// 保留各执行器的原始字段口径；NULL 表示该字段从未实报，已知 0 则返回 0。
fn provider_reports(c: &Connection, room: &str) -> Result<Vec<Value>> {
    let columns = TOKEN_FIELDS
        .iter()
        .flat_map(|(field, _)| {
            [format!("SUM(json_extract(reported,'$.{field}'))"), format!("COUNT(json_extract(reported,'$.{field}'))")]
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT backend,model,COUNT(*),SUM(reported IS NULL),{columns}
        FROM turn_usage_attempts WHERE room_id=? GROUP BY backend,model ORDER BY backend,model"
    );
    Ok(c.prepare(&sql)?
        .query_map([room], |r| {
            let mut totals = Map::new();
            let mut reported = Map::new();
            for (index, (field, _)) in TOKEN_FIELDS.iter().enumerate() {
                totals.insert((*field).into(), json!(r.get::<_, Option<i64>>(4 + index * 2)?));
                reported.insert((*field).into(), json!(r.get::<_, i64>(5 + index * 2)?));
            }
            Ok(json!({"backend":r.get::<_, String>(0)?,"model":r.get::<_, Option<String>>(1)?,
            "attempts":r.get::<_, i64>(2)?,"missingReports":r.get::<_, i64>(3)?,"tokens":totals,"reported":reported}))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工可复算：同投递两种后端各记一次，零值保留，缺报、重复完成和重启区分。
    #[test]
    fn stage_usage_keeps_known_totals_and_coverage_separate() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        for (id, room, backend, source) in [
            ("first", Some("phase"), "alpha", "user"),
            ("fallback", Some("phase"), "beta", "user"),
            ("crash", Some("phase"), "beta", "maintenance"),
            ("private", None, "beta", "shared_coordinator"),
        ] {
            store
                .begin_usage_attempt(&AttemptStart {
                    id,
                    bot: "a",
                    room,
                    delivery: room.map(|_| "stable-delivery"),
                    backend,
                    source,
                    requests: &[],
                })
                .unwrap();
        }
        store.observe_usage_event("first", UsageEvent::Process, None).unwrap();
        store.finish_usage_attempt("first", "failed", 5, &Value::Null).unwrap();
        store.observe_usage_event("fallback", UsageEvent::Process, None).unwrap();
        store.observe_usage_event("fallback", UsageEvent::Session, None).unwrap();
        store.observe_usage_event("fallback", UsageEvent::Prompt, Some("reported-model")).unwrap();
        let reported = json!({"source":"provider_reported","inputTokens":0,"cachedReadTokens":14,"outputTokens":8});
        store.finish_usage_attempt("fallback", "noReply", 20, &reported).unwrap();
        store
            .finish_usage_attempt(
                "fallback",
                "completed",
                100,
                &json!({"source":"provider_reported","inputTokens":100}),
            )
            .unwrap();
        store.interrupt_usage_attempts().unwrap();
        store.finish_usage_attempt("crash", "completed", 40, &reported).unwrap();
        let stats = store.stage_usage("phase").unwrap();
        assert_eq!(
            stats["counts"],
            json!({"attempts":3,"prompts":1,"sessions":1,"processes":2,"missingReports":2,"durationMs":25})
        );
        assert_eq!(stats["outcomes"], json!({"failed":1,"noReply":1,"interrupted":1}));
        assert_eq!(stats["triggers"]["maintenance"], 1);
        assert_eq!(stats["unassigned"]["shared_coordinator"], 1);
        assert!(stats["providers"][0]["model"].is_null());
        assert_eq!(stats["providers"][2]["model"], "reported-model");
        assert_eq!(
            stats["providers"][2]["tokens"],
            json!({"inputTokens":0,"cachedReadTokens":14,"cachedWriteTokens":null,"outputTokens":8,"thoughtTokens":null})
        );
        assert_eq!(stats["providers"][2]["reported"]["cachedWriteTokens"], 0);
        assert_eq!(stats["providers"][2]["reported"]["inputTokens"], 1);
    }
}
