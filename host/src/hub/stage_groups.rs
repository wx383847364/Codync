//! 从确认的角色配置创建阶段群，批量保存身份与创建回执。

use super::{Hub, validate};
use crate::LockExt;
use crate::agent::{backends, fallback, workspace};
use crate::chat::files;
use crate::store::{BotConfig, StageGroup, StageHandoff, now_ms};
use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Draft {
    bot_id: String,
    request_id: String,
    name: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    acceptance: String,
    leader_id: String,
    roles: Vec<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handoff_version: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Role {
    bot_id: String,
    source_rev: i64,
    description: String,
    #[serde(default)]
    reuse_project: bool,
}

#[derive(Deserialize, Serialize)]
struct Receipt {
    digest: String,
    room: String,
    members: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum Change {
    SaveHandoff { markdown: String, evidence: String, previous_version: Option<String> },
    Accept { version: String },
    Close,
    Reopen,
}

impl Hub {
    /// 已保存的回执和无需连接器的角色可以在凭据库未打开时处理。
    pub(crate) async fn create_stage_group(self: &Arc<Self>, body: Value) -> Result<Value> {
        let draft: Draft = serde_json::from_value(body).context("invalid stage group draft")?;
        let request = Uuid::parse_str(&draft.request_id).context("invalid stage creation ID")?;
        let key = format!("stage.creation.{request}");
        if self.store.kv_read(&key)?.is_none() {
            for role in &draft.roles {
                if self
                    .store
                    .bot(&role.bot_id)?
                    .is_some_and(|row| !row.deleted && row.rev == role.source_rev && !row.config.connectors.is_empty())
                {
                    crate::market::vault::unlock(self.clone())
                        .await
                        .context("open connector access for the selected roles")?;
                    break;
                }
            }
        }
        let hub = self.clone();
        tokio::task::spawn_blocking(move || hub.save_stage_group(draft, &key)).await?
    }

    /// 同次确认只创建一组身份；在写入成功前不启动 actor 或发布同步。
    fn save_stage_group(self: &Arc<Self>, draft: Draft, key: &str) -> Result<Value> {
        let digest = files::digest_hex(&Sha256::digest(serde_json::to_vec(&draft)?));
        let room = Uuid::new_v4().to_string();
        // 大文件复制和 fsync 在发布锁外准备；提交前重新核对已确认的版本。
        let prepared = (|| {
            if draft.handoff_version.is_none() || self.store.kv_read(key)?.is_some() {
                return Ok((None, vec![]));
            }
            let source = self.store.bot(&draft.bot_id)?.filter(|row| !row.deleted).context("source group not found")?;
            incoming_archive(&source.config, draft.handoff_version.as_deref(), &room)
        })();
        let (room, members) = {
            let _guard = self.emit_lock.locked();
            if let Some(saved) = self.store.kv_read(key)? {
                let receipt: Receipt = serde_json::from_str(&saved)?;
                ensure!(receipt.digest == digest, "this creation ID was already used for a different stage group");
                let bot = self
                    .bot_value(&receipt.room)
                    .filter(|bot| bot["deleted"] != true)
                    .ok_or_else(|| anyhow!("this stage group was deleted"))?;
                return Ok(stage_result(&bot, &receipt.members, true));
            }
            ensure!(
                !draft.name.trim().is_empty() && draft.name.len() <= 512,
                "a stage name is required (512 bytes maximum)"
            );
            ensure!(
                draft.goal.len() + draft.acceptance.len() <= 24_000,
                "goal and acceptance criteria exceed 24,000 bytes combined"
            );
            let source = self
                .store
                .bot(&draft.bot_id)?
                .filter(|row| !row.deleted && row.config.is_group())
                .ok_or_else(|| anyhow!("source group not found"))?
                .config;
            let selected: HashSet<_> = draft.roles.iter().map(|role| role.bot_id.as_str()).collect();
            ensure!(
                selected.len() == draft.roles.len() && selected.contains(draft.leader_id.as_str()),
                "choose a lead from the selected roles"
            );
            ensure!(
                draft.roles.iter().all(|role| source.members.contains(&role.bot_id)),
                "one of the selected roles left the source group"
            );

            let position = self.new_bot_position()?;
            let mut bots = Vec::with_capacity(draft.roles.len() + 1);
            for role in &draft.roles {
                let original = self
                    .store
                    .bot(&role.bot_id)?
                    .filter(|row| !row.deleted && !row.config.is_group())
                    .ok_or_else(|| anyhow!("one of the selected roles was deleted"))?;
                // 目录及授权属于同一次确认。预览期间发生变化时请用户重新查看角色。
                ensure!(
                    original.rev == role.source_rev,
                    "{} changed while you were reviewing the role; reopen the form",
                    original.config.name
                );
                bots.push(self.stage_role(&original.config, role, position)?);
            }
            let leader =
                draft.roles.iter().position(|role| role.bot_id == draft.leader_id).context("choose a stage lead")?;
            let handoff = accepted_handoff(&source, draft.handoff_version.as_deref())?;
            let (mut archive, mut evidence_files) = prepared?;
            ensure!(handoff.is_some() == archive.is_some(), "The accepted handoff changed. Review it and try again.");
            let stage = StageGroup {
                leader_id: bots[leader].id.clone(),
                goal: draft.goal,
                acceptance: draft.acceptance,
                incoming: archive.as_ref().map(|snapshot| snapshot.meta.clone()),
                incoming_files: evidence_files.iter().map(|snapshot| snapshot.meta.clone()).collect(),
                ..StageGroup::default()
            };
            bots.push(serde_json::from_value(json!({
                "id": room, "kind": "group", "name": draft.name.trim(), "position": position,
                "members": bots.iter().map(|bot| &bot.id).collect::<Vec<_>>(),
                "section": source.section, "stage": stage, "createdAt": now_ms()
            }))?);
            let members = bots.iter().filter(|cfg| !cfg.is_group()).map(|cfg| cfg.id.clone()).collect::<Vec<_>>();
            let receipt = serde_json::to_string(&Receipt { digest, room: room.clone(), members: members.clone() })?;
            self.store.save_bots_with_creation(&bots, Some((key, &receipt)))?;
            if let Some(snapshot) = archive.as_mut() {
                snapshot.commit();
            }
            for file in &mut evidence_files {
                file.commit();
            }
            // 全新 ID 没有回退或快模式运行态，注册 actor 不会同步回发运行态事件。
            // 发群事件前注册所有成员，避免首条消息比 actor 注册更早进入调度。
            for cfg in bots.iter().filter(|cfg| !cfg.is_group()) {
                self.spawn_bot(cfg.clone());
            }
            // 广播使用已提交的 rev 顺序，重连可直接读取同样的名单和负责人。
            for cfg in &bots {
                if let Some(bot) = self.bot_value(&cfg.id) {
                    let _ = self.events.send(json!({"type":"bot", "rev":bot["rev"], "bot":bot}));
                }
            }
            (room, members)
        };
        crate::network::publish(self);
        crate::usage::bots_changed(self);
        let bot = self.bot_value(&room).context("new stage group could not be read")?;
        Ok(stage_result(&bot, &members, false))
    }

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
                        ensure!(stage.closed_at.is_none(), "Reopen the stage to edit its handoff.");
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
                        let handoff = stage.handoff.as_mut().filter(|handoff| handoff.file.id == version)
                            .context("Save and review the current handoff before accepting the stage.")?;
                        ensure!(!handoff.evidence_files.is_empty() || durable_evidence(&handoff.evidence), "Add a durable evidence reference before accepting the stage.");
                        for file in &handoff.evidence_files { files::readable_path(room, file)?; }
                        files::read_markdown(room, &handoff.file)?;
                        ensure!(!pending, "Finish or stop pending work before accepting the stage.");
                        handoff.accepted_at.get_or_insert_with(now_ms);
                    }
                    Change::Close => {
                        ensure!(!pending, "Finish or stop pending work before closing the stage.");
                        stage.closed_at.get_or_insert_with(now_ms);
                    }
                    Change::Reopen => stage.closed_at = None,
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

    /// 只复制用户确认的角色白名单，托管目录按新身份保留路径并在首次运行时建立。
    fn stage_role(&self, source: &BotConfig, role: &Role, position: i64) -> Result<BotConfig> {
        ensure!(role.description.len() <= 16_000, "role instructions are too long");
        // 自定义命令可能内嵌环境变量或凭据，阶段实例使用已安装的标准代理入口。
        ensure!(
            source.command.is_none() && source.fallbacks.iter().all(|item| item.command.is_none()),
            "{} uses a custom launch command; choose a role with a standard agent configuration",
            source.name
        );
        backends::launch_candidates(&source.backend)?;
        ensure!(
            backends::signed_in(&source.backend) != Some(false),
            "sign in to {} for {}",
            source.backend,
            source.name
        );
        for fallback in &source.fallbacks {
            backends::launch_candidates(&fallback.backend)?;
        }
        let skills = crate::market::skills(&self.store);
        // 私有技能不会被新实例继承，公共技能及连接器以当前授权为准。
        let mut selected_skills = Vec::new();
        for id in &source.skills {
            let skill = skills
                .iter()
                .find(|skill| &skill.id == id)
                .with_context(|| format!("skill {id} for {} is unavailable; reinstall or deselect it", source.name))?;
            if skill.written_by.is_none() {
                selected_skills.push(id);
            }
        }
        if !source.connectors.is_empty() {
            let available =
                crate::market::list_connectors(&self.store).context("check the selected role's connector access")?;
            for id in &source.connectors {
                let connector = available["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|item| item["id"].as_str() == Some(id.as_str()))
                    .with_context(|| {
                        format!("connector {id} for {} is unavailable; reconnect or deselect it", source.name)
                    })?;
                ensure!(
                    connector["auth"] != "signedOut",
                    "sign in to {} for {}",
                    connector["name"].as_str().unwrap_or(id),
                    source.name
                );
            }
        }
        let id = Uuid::new_v4().to_string();
        let project = role.reuse_project && !workspace::is_managed(source) && !source.cwd.is_empty();
        ensure!(!role.reuse_project || project, "{} has no shared project folder to reuse", source.name);
        let mut cfg: BotConfig = serde_json::from_value(json!({
            "id":id, "name":source.name, "description":role.description,
            "avatarColor":source.avatar_color, "avatarShape":source.avatar_shape,
            "backend":source.backend, "model":source.model, "modelOptions":source.model_options,
            "mode":source.mode, "fastMode":source.fast_mode, "fallbacks":source.fallbacks,
            "selfFallbacks":source.self_fallbacks, "permission":source.permission,
            "cwd":if project {source.cwd.clone()} else {String::new()},
            "skills":selected_skills, "connectors":source.connectors,
            "position":position, "section":source.section, "createdAt":now_ms(),
            "offeredToNetwork":false, "reachableByAssistants":false, "sharedMemory":false, "harnessMemory":false
        }))?;
        fallback::tidy(&mut cfg);
        validate(&cfg)?;
        if !project {
            cfg.cwd = workspace::path(&id).to_string_lossy().into_owned();
        }
        Ok(cfg)
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

/// 返回实时群配置与本次创建的稳定成员回执，便于断线重试和恢复草稿。
fn stage_result(bot: &Value, members: &[String], replayed: bool) -> Value {
    let draft = initial_draft(
        bot["stage"]["goal"].as_str().unwrap_or_default(),
        bot["stage"]["acceptance"].as_str().unwrap_or_default(),
    );
    json!({"bot":bot,"draft":draft,"createdMembers":members,"replayed":replayed})
}

/// 从持久阶段信息重建首条草稿，调用方可在断线后恢复该内容。
fn initial_draft(goal: &str, acceptance: &str) -> String {
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

/// 将用户选中的已验收版本复制到新群，旧群后续变更不能替换新群材料。
fn incoming_archive(
    source: &BotConfig,
    version: Option<&str>,
    room: &str,
) -> Result<(Option<files::Snapshot>, Vec<files::Snapshot>)> {
    let Some(handoff) = accepted_handoff(source, version)? else { return Ok((None, vec![])) };
    let markdown = files::read_markdown(&source.id, &handoff.file)?;
    let archive = files::snapshot_bytes(&files::root(room), &handoff.file.name, markdown.as_bytes())?;
    let evidence =
        handoff.evidence_files.iter().map(|file| files::copy_shared(&source.id, room, file)).collect::<Result<_>>()?;
    Ok((Some(archive), evidence))
}

/// 只接受用户预览过的已验收版本，准备副本期间源配置仍可更新。
fn accepted_handoff<'a>(source: &'a BotConfig, version: Option<&str>) -> Result<Option<&'a StageHandoff>> {
    let Some(stage) = &source.stage else {
        ensure!(version.is_none(), "The source group has no confirmed handoff.");
        return Ok(None);
    };
    stage
        .handoff
        .as_ref()
        .filter(|handoff| handoff.accepted_at.is_some() && version == Some(handoff.file.id.as_str()))
        .map(Some)
        .context("Review and select the accepted handoff before creating the next stage.")
}
