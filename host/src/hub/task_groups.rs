//! 从确认的角色配置创建独立会话或阶段群，批量保存身份与创建回执。

use super::stage_groups::initial_draft;
use super::{Hub, validate};
use crate::LockExt;
use crate::agent::{backends, fallback, workspace};
use crate::chat::files;
use crate::store::{BotConfig, PreviousStage, StageGroup, StageHandoff, now_ms};
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
    #[serde(default)]
    name: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    acceptance: String,
    #[serde(default)]
    leader_id: String,
    roles: Vec<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handoff_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_previous: Option<bool>,
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

impl Hub {
    /// 已保存的回执和无需连接器的角色可以在凭据库未打开时处理。
    pub(crate) async fn create_task_group(self: &Arc<Self>, body: Value, independent: bool) -> Result<Value> {
        let track = independent || body["referencePrevious"] == true;
        let result = self.create_task_group_inner(body, independent).await;
        if track {
            // 不采集群名、角色内容、查询或原文。
            crate::analytics::capture(
                self,
                crate::analytics::Event::TaskGroupCreated,
                serde_json::Map::from_iter([
                    ("kind".into(), json!(if independent { "independent" } else { "stage" })),
                    ("outcome".into(), json!(if result.is_ok() { "saved" } else { "failed" })),
                    ("category".into(), json!(result.is_err().then_some("creation"))),
                ]),
            );
        }
        result
    }

    /// 解析、校验并提交群草稿，调用方记录无内容的事件结果。
    async fn create_task_group_inner(self: &Arc<Self>, body: Value, independent: bool) -> Result<Value> {
        let draft: Draft = serde_json::from_value(body).context("invalid stage group draft")?;
        let request = Uuid::parse_str(&draft.request_id).context("invalid stage creation ID")?;
        ensure!(
            !independent || (draft.reference_previous != Some(true) && draft.handoff_version.is_none()),
            "An independent session cannot reference a previous stage."
        );
        let namespace = if independent { "group.session.creation" } else { "stage.creation" };
        let key = format!("{namespace}.{request}");
        if self.store.kv_read(&key)?.is_none() {
            for role in &draft.roles {
                if self.store.bot(&role.bot_id)?.is_some_and(|row| {
                    !row.deleted && row.matches_config_at(role.source_rev) && !row.config.connectors.is_empty()
                }) {
                    crate::market::vault::unlock(self.clone())
                        .await
                        .context("open connector access for the selected roles")?;
                    break;
                }
            }
        }
        let hub = self.clone();
        tokio::task::spawn_blocking(move || hub.save_task_group(draft, &key, independent)).await?
    }

    /// 同次确认只创建一组身份；在写入成功前不启动 actor 或发布同步。
    fn save_task_group(self: &Arc<Self>, draft: Draft, key: &str, independent: bool) -> Result<Value> {
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
                    .ok_or_else(|| anyhow!("this task group was deleted"))?;
                return Ok(task_group_result(&bot, &receipt.members, true));
            }
            ensure!(draft.name.len() <= 512, "stage name exceeds 512 bytes");
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
            ensure!(!selected.is_empty(), "Choose at least one bot.");
            ensure!(selected.len() == draft.roles.len(), "Select each role only once.");
            ensure!(
                draft.leader_id.is_empty() || selected.contains(draft.leader_id.as_str()),
                "choose a lead from the selected roles"
            );
            ensure!(
                independent || draft.roles.iter().all(|role| source.members.contains(&role.bot_id)),
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
                // 目录及授权等配置必须与预览一致，运行态与已读变化允许继续。
                ensure!(
                    original.matches_config_at(role.source_rev),
                    "{} changed while you were reviewing the role; reopen the form",
                    original.config.name
                );
                bots.push(self.copy_task_role(
                    &original.config,
                    role,
                    position,
                    !independent && draft.reference_previous != Some(true),
                )?);
            }
            // 空字符串保留既有协议类型，表示此阶段默认询问所有成员。
            let leader_id = draft
                .roles
                .iter()
                .position(|role| role.bot_id == draft.leader_id)
                .map(|index| bots[index].id.clone())
                .unwrap_or_default();
            let members: Vec<_> = bots.iter().map(|bot| bot.id.clone()).collect();
            let mut cfg = json!({
                "id": room, "kind": "group", "position": position,
                "members": members, "section": source.section, "createdAt": now_ms()
            });
            let supplied_name = draft.name.trim();
            if independent {
                let (base, numbered) = self.group_session_name(&source)?;
                cfg["name"] = json!(if supplied_name.is_empty() { numbered.as_str() } else { supplied_name });
                cfg["groupSessionBase"] = json!(base);
                cfg["groupSessionOwnedMembers"] = json!(members);
                cfg["groupLeaderId"] = json!((!leader_id.is_empty()).then_some(&leader_id));
            } else {
                cfg["name"] = json!(if supplied_name.is_empty() {
                    format!(
                        "{} · {}",
                        source.name,
                        bots.iter().map(|bot| bot.name.as_str()).collect::<Vec<_>>().join(", ")
                    )
                    .chars()
                    .scan(0, |bytes, ch| {
                        *bytes += ch.len_utf8();
                        (*bytes <= 512).then_some(ch)
                    })
                    .collect::<String>()
                } else {
                    supplied_name.to_owned()
                });
            }
            let handoff = accepted_handoff(&source, draft.handoff_version.as_deref())?;
            if draft.reference_previous == Some(true) {
                // 交接在创建页面预览，提交时重新核对版本；旧客户端仍可主动跳过。
                ensure!(
                    handoff.is_some()
                        == source
                            .stage
                            .as_ref()
                            .and_then(|stage| stage.handoff.as_ref())
                            .is_some_and(|handoff| handoff.accepted_at.is_some()),
                    "The accepted handoff changed. Reopen the form and review it."
                );
            }
            let (mut archive, mut evidence_files) = prepared?;
            ensure!(handoff.is_some() == archive.is_some(), "The accepted handoff changed. Review it and try again.");
            if !independent {
                cfg["stage"] = json!(StageGroup {
                    leader_id,
                    goal: draft.goal,
                    acceptance: draft.acceptance,
                    owned_members: members.clone(),
                    incoming: archive.as_ref().map(|snapshot| snapshot.meta.clone()),
                    incoming_files: evidence_files.iter().map(|snapshot| snapshot.meta.clone()).collect(),
                    previous: (draft.reference_previous == Some(true)).then(|| PreviousStage {
                        room_id: source.id.clone(),
                        name: source.name.clone(),
                        cutoff_rev: 0,
                    }),
                    ..StageGroup::default()
                });
            }
            bots.push(serde_json::from_value(cfg)?);
            let receipt = serde_json::to_string(&Receipt { digest, room: room.clone(), members: members.clone() })?;
            self.store.save_created_group(&mut bots, (key, &receipt))?;
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
        let bot = self.bot_value(&room).context("new task group could not be read")?;
        Ok(task_group_result(&bot, &members, false))
    }

    /// 只复制用户确认的角色白名单，托管目录按新身份保留路径并在首次运行时建立。
    fn copy_task_role(&self, source: &BotConfig, role: &Role, position: i64, legacy_stage: bool) -> Result<BotConfig> {
        ensure!(role.description.len() <= 16_000, "role instructions are too long");
        // 自定义命令可能内嵌环境变量或凭据，阶段实例使用已安装的标准代理入口。
        ensure!(
            source.task_role_copyable(),
            "{} uses a custom launch command; choose a role with a standard agent configuration",
            source.name
        );
        // 新表单只保存配置，安装和登录检查由实际派工触发；旧调用维持原有预检。
        if legacy_stage {
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

/// 返回实时群配置与本次创建的稳定成员回执，便于断线重试和恢复草稿。
fn task_group_result(bot: &Value, members: &[String], replayed: bool) -> Value {
    let draft = initial_draft(
        bot["stage"]["goal"].as_str().unwrap_or_default(),
        bot["stage"]["acceptance"].as_str().unwrap_or_default(),
    );
    json!({"bot":bot,"draft":draft,"createdMembers":members,"replayed":replayed})
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
    // 只有用户明确选择交接时，才校验来源阶段与验收版本。
    let Some(version) = version else { return Ok(None) };
    source
        .stage
        .as_ref()
        .and_then(|stage| stage.handoff.as_ref())
        .filter(|handoff| handoff.accepted_at.is_some() && version == handoff.file.id)
        .map(Some)
        .context("Review and select the accepted handoff before creating the next stage.")
}
