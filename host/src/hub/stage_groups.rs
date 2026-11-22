//! 阶段详情、交接验收与归档。

use super::Hub;
use crate::LockExt;
use crate::chat::files;
use crate::store::{StageGroup, StageHandoff, now_ms};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum Change {
    SaveHandoff { markdown: String, evidence: String, previous_version: Option<String> },
    Accept { version: String },
    Close,
    Reopen,
    Archive,
    Restore,
}

impl Hub {
    /// 读取当前阶段、交接和队列状态，查看流程不向机器人发送工作。
    pub(crate) fn stage_details(&self, room: &str) -> Result<Value> {
        let _guard = self.emit_lock.locked();
        let bot = self
            .bot_value(room)
            .filter(|bot| bot["deleted"] != true && bot["stage"].is_object())
            .context("stage group not found")?;
        let stage: StageGroup = serde_json::from_value(bot["stage"].clone())?;
        // 编辑器单独呈现证据字段，保存后的完整归档仍包含该字段。
        let current = stage.handoff.as_ref().map(|handoff| {
            let content = files::read_markdown(room, &handoff.file)?;
            Ok::<_, anyhow::Error>(
                content.strip_suffix(&evidence_section(&handoff.evidence)).unwrap_or(&content).to_owned(),
            )
        });
        let incoming = stage.incoming.as_ref().map(|file| files::read_markdown(room, file));
        Ok(
            json!({"bot":bot,"work":self.store.stage_work(room)?,"initialDraft":initial_draft(&stage.goal,&stage.acceptance),
                "markdown":current.as_ref().and_then(|result| result.as_ref().ok()),
                "handoffUnavailable":current.as_ref().is_some_and(Result::is_err)
                    || stage.handoff.as_ref().is_some_and(|handoff| handoff.evidence_files.iter().any(|file| files::readable_path(room, file).is_err())),
                "incomingMarkdown":incoming.as_ref().and_then(|result| result.as_ref().ok()),
                "incomingUnavailable":incoming.as_ref().is_some_and(Result::is_err)
                    || stage.incoming_files.iter().any(|file| files::readable_path(room, file).is_err()),
                "sharedFiles":self.stage_file_choices(room,&stage)?,
                "template":format!("# Stage handoff\n\n## Goal and result\n{}\n\n## Acceptance criteria\n{}\n\n## User requirements and prohibited changes\n\n## Stable constraints and confirmed decisions\n\n## Code, resources and versions\n\n## Evidence\n\n## Open questions and known issues\n\n## Next stage scope and frozen items\n",stage.goal,stage.acceptance)
            }),
        )
    }

    /// 保存人工交接、单独验收或切换准入；所有操作只由人发起。
    pub(crate) fn change_stage(&self, room: &str, body: Value) -> Result<Value> {
        let change: Change = serde_json::from_value(body).context("invalid stage action")?;
        // 对大文件的内容校验先在写锁外完成，事务内再核对所验收的版本。
        if let Change::Accept { version } = &change
            && let Some(handoff) = self
                .store
                .bot(room)?
                .and_then(|row| row.config.stage)
                .and_then(|stage| stage.handoff)
                .filter(|handoff| handoff.file.id == *version)
        {
            for file in &handoff.evidence_files {
                files::verify_shared(room, file)?;
            }
        }
        {
            let _guard = self.emit_lock.locked();
            let mut archive = None;
            let evidence_files = if let Change::SaveHandoff { evidence, .. } = &change {
                self.select_evidence_files(room, evidence)?
            } else {
                vec![]
            };
            self.store.update_stage(room, |stage, pending| {
                match change {
                    Change::SaveHandoff { markdown, evidence, previous_version } => {
                        ensure!(!stage.read_only(), "Reopen the stage to edit its handoff.");
                        let markdown = markdown.trim();
                        let evidence = evidence.trim();
                        ensure!(!markdown.is_empty() && markdown.len() <= 64_000 && evidence.len() <= 8_000,
                            "Add handoff Markdown (64,000 bytes maximum) and keep evidence references within 8,000 bytes.");
                        let content = format!("{markdown}{}", evidence_section(evidence));
                        let digest = files::digest_hex(&Sha256::digest(content.as_bytes()));
                        if stage.handoff.as_ref().is_some_and(|handoff| handoff.file.sha256 == digest && handoff.evidence == evidence
                            && files::read_markdown(room, &handoff.file).is_ok()) {
                            return Ok(());
                        }
                        ensure!(stage.handoff.as_ref().map(|handoff| handoff.file.id.as_str()) == previous_version.as_deref(),
                            "The handoff changed on another device. Reopen stage details and review the saved version.");
                        let saved = files::snapshot_bytes(&files::root(room), "stage-handoff.md", content.as_bytes())?;
                        stage.handoff = Some(StageHandoff {file:saved.meta.clone(), evidence:evidence.into(), evidence_files, saved_at:now_ms(), accepted_at:None});
                        archive = Some(saved);
                    }
                    Change::Accept { version } => {
                        ensure!(stage.archived_at.is_none(), "Restore the stage before accepting its handoff.");
                        let handoff = stage.handoff.as_mut().filter(|handoff| handoff.file.id == version)
                            .context("Save and review the current handoff before accepting the stage.")?;
                        ensure!(!handoff.evidence_files.is_empty() || durable_evidence(&handoff.evidence), "Add a durable evidence reference before accepting the stage.");
                        for file in &handoff.evidence_files { files::readable_path(room, file)?; }
                        files::read_markdown(room, &handoff.file)?;
                        ensure!(!pending, "Finish or stop pending work before accepting the stage.");
                        handoff.accepted_at.get_or_insert_with(now_ms);
                    }
                    Change::Close => {
                        ensure!(stage.archived_at.is_none(), "Restore the stage before closing it.");
                        ensure!(!pending, "Finish or stop pending work before closing the stage.");
                        stage.closed_at.get_or_insert_with(now_ms);
                    }
                    Change::Reopen => {
                        ensure!(stage.archived_at.is_none(), "Restore the stage before reopening it.");
                        stage.closed_at = None;
                    }
                    Change::Archive => {
                        ensure!(!pending, "Finish or stop pending work before archiving the stage.");
                        stage.archived_at.get_or_insert_with(now_ms);
                    }
                    Change::Restore => stage.archived_at = None,
                }
                Ok(())
            })?;
            if let Some(snapshot) = archive.as_mut() {
                snapshot.commit();
            }
            if let Some(bot) = self.bot_value(room) {
                let _ = self.events.send(json!({"type":"bot","rev":bot["rev"],"bot":bot}));
            }
        }
        self.stage_details(room)
    }

    /// 复用本群已发布的文件快照；私聊和其他群的材料需要先明确分享到这里。
    fn stage_file_choices(&self, room: &str, stage: &StageGroup) -> Result<Vec<files::SharedFile>> {
        let mut choices = Vec::new();
        let mut ids = HashSet::new();
        for file in pinned_files(stage) {
            if ids.insert(file.id.clone()) {
                choices.push(file.clone());
            }
        }
        for message in self.store.file_messages(room, 100)? {
            for file in message.data["files"].as_array().into_iter().flatten() {
                if let Ok(file) = serde_json::from_value::<files::SharedFile>(file.clone())
                    && ids.insert(file.id.clone())
                {
                    choices.push(file);
                }
            }
        }
        Ok(choices)
    }

    /// 精确校验用户选中的版本，不受界面列表页数影响。
    fn select_evidence_files(&self, room: &str, evidence: &str) -> Result<Vec<files::SharedFile>> {
        let stage = self.store.bot(room)?.and_then(|row| row.config.stage).context("stage group not found")?;
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for id in evidence.lines().filter_map(|line| line.trim().strip_prefix("file:").map(str::trim)) {
            if seen.insert(id) {
                let file = pinned_files(&stage)
                    .find(|file| file.id == id)
                    .cloned()
                    .map_or_else(|| self.store.stage_shared_file(room, id), |file| Ok(Some(file)))?
                    .context("Choose an available shared evidence file in this stage.")?;
                files::readable_path(room, &file)?;
                result.push(file);
            }
        }
        Ok(result)
    }
}

/// 临时目录链接不能作为唯一验收依据；人工确认其余提交、路径或持久链接。
fn durable_evidence(evidence: &str) -> bool {
    evidence.lines().any(|line| {
        let normalized = line.to_ascii_lowercase().replace('\\', "/");
        // 普通说明不能代替实际链接、路径或 Git 版本。
        if ["/tmp/", "/temp/", "file:///tmp/"].iter().any(|path| normalized.contains(path)) {
            return false;
        }
        line.split_whitespace().any(|word| {
            let word = word.trim_matches(['`', '(', ')', '[', ']', ',', '.']);
            word.starts_with("https://")
                || word.split_once("](https://").is_some()
                || (word.len() >= 6 && word.len() <= 64 && word.bytes().all(|c| c.is_ascii_hexdigit()))
                || (word.starts_with('/') && std::path::Path::new(word).is_file())
        })
    })
}

/// 合并已固定的出站和入站证据，后续编辑可引用历史固定版本。
fn pinned_files(stage: &StageGroup) -> impl Iterator<Item = &files::SharedFile> {
    stage.incoming_files.iter().chain(stage.handoff.iter().flat_map(|handoff| &handoff.evidence_files))
}

/// 从持久阶段信息重建首条草稿，调用方可在断线后恢复该内容。
pub(super) fn initial_draft(goal: &str, acceptance: &str) -> String {
    let goal = goal.trim();
    let acceptance = acceptance.trim();
    match (goal.is_empty(), acceptance.is_empty()) {
        (_, true) => goal.to_owned(),
        (true, false) => format!("Acceptance criteria\n{acceptance}"),
        (false, false) => format!("{goal}\n\nAcceptance criteria\n{acceptance}"),
    }
}

/// 证据引用固定在交接文件里，下一阶段读取的是同一个版本。
fn evidence_section(evidence: &str) -> String {
    if evidence.is_empty() { String::new() } else { format!("\n\n## Evidence references\n\n{evidence}\n") }
}
