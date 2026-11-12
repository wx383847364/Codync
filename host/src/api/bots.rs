//! Bot and chat methods: roster, messages, uploads, memory, artifacts, routines and the bots' own MCP calls.

use super::devices::Caller;
use super::str_arg;
use crate::agent::bot::Cmd;
use crate::hub::Hub;
use crate::store::{EntryKind, Lane};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

/// 处理客户端意图，提及校验先于裁剪，投递生命周期由 host 统一执行。
pub(super) async fn call(hub: &Arc<Hub>, caller: &Caller, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "routineSchedule" => crate::routines::schedule_preview(&b, crate::store::now_ms())?,
        "routines" => hub.routines.list(str_arg(&b, "botId")?),
        "saveRoutine" => hub.routines.save(hub, str_arg(&b, "botId")?, &b)?,
        "setRoutineEnabled" => hub.routines.set_enabled(
            &hub.store,
            str_arg(&b, "botId")?,
            str_arg(&b, "id")?,
            b["enabled"].as_bool().context("enabled is required")?,
        )?,
        "deleteRoutine" => hub.routines.remove(hub, str_arg(&b, "botId")?, str_arg(&b, "id")?)?,
        "runRoutine" => hub.routines.enqueue(
            &hub.store,
            str_arg(&b, "botId")?,
            str_arg(&b, "id")?,
            json!({"source":"test"}),
            None,
            true,
        )?,
        "retryRoutineRun" => hub.routines.retry(hub, str_arg(&b, "botId")?, str_arg(&b, "runId")?)?,
        "routineWebhook" => hub.routines.credentials(
            hub,
            str_arg(&b, "botId")?,
            str_arg(&b, "id")?,
            b["rotate"].as_bool().unwrap_or(false),
        )?,
        "routineCall" => crate::routines::call(hub, str_arg(&b, "botId")?, str_arg(&b, "name")?, &b["arguments"])?,
        "memoryCall" => {
            crate::chat::memory::tool_call(
                hub,
                str_arg(&b, "botId")?,
                str_arg(&b, "name")?,
                b["groupSession"].as_str(),
                &b["arguments"],
            )
            .await?
        }
        "chatCall" => {
            let (bot, name) = (str_arg(&b, "botId")?, str_arg(&b, "name")?);
            if crate::chat::profile::TOOLS.contains(&name) {
                crate::chat::profile::call(hub, bot, name, &b["arguments"]).await?
            } else if name == crate::chat::skills::TOOL {
                crate::chat::skills::call(hub, bot, &b["arguments"]).await?
            } else if crate::chat::fallbacks::TOOLS.contains(&name) {
                crate::chat::fallbacks::call(hub, bot, name, &b["arguments"]).await?
            } else if crate::chat::artifacts::TOOLS.contains(&name) {
                crate::chat::artifacts::call(hub, bot, name, b["groupSession"].as_str(), &b["arguments"]).await?
            } else if crate::dev_servers::TOOLS.contains(&name) {
                crate::dev_servers::call(hub, bot, name, b["groupSession"].as_str(), &b["arguments"]).await?
            } else {
                crate::chat::outbox::call(hub, bot, name, b["groupSession"].as_str(), &b["arguments"]).await?
            }
        }
        // An agent on this computer that isn't a bot (`codync-host mcp network`).
        "agentCall" => {
            crate::chat::team::assistant::local_call(hub, str_arg(&b, "botId")?, str_arg(&b, "name")?, &b["arguments"])
                .await?
        }
        "teamCall" => {
            crate::chat::team::call_async(
                hub,
                str_arg(&b, "botId")?,
                str_arg(&b, "name")?,
                b["groupSession"].as_str(),
                &b["arguments"],
            )
            .await?
        }
        "manageCall" => {
            crate::chat::manage::call(
                hub,
                str_arg(&b, "botId")?,
                str_arg(&b, "name")?,
                b["groupSession"].as_str(),
                &b["arguments"],
            )
            .await?
        }
        "history" => {
            let bot = str_arg(&b, "botId")?;
            let before = b["beforeSeq"].as_i64().unwrap_or(i64::MAX);
            let limit = b["limit"].as_i64().unwrap_or(100).clamp(1, 500);
            json!({"entries": hub.store.history(bot, before, limit)?})
        }
        // Notices between a bot and one peer (the newest 200), for the read-only bot conversation sheet.
        "botConversation" => {
            json!({"entries": hub.store.bot_conversation(str_arg(&b, "botId")?, str_arg(&b, "peerId")?, 200)?})
        }
        "createBot" => {
            let mut b = b;
            // Only a bot's `create_bot` records who made one.
            b["createdBy"] = Value::Null;
            let cfg = crate::agent::new_bot::with_defaults(hub, b).await?;
            let hub = hub.clone();
            json!({"bot": tokio::task::spawn_blocking(move || hub.create_bot(cfg)).await??})
        }
        "createGroupSession" => {
            let id = str_arg(&b, "botId")?.to_owned();
            let selection: Option<crate::hub::GroupSessionSelection> =
                serde_json::from_value(b["selection"].clone()).context("invalid group session selection")?;
            let hub = hub.clone();
            // 旧请求省略 selection，由 Host 沿用来源群成员和普通群负责人。
            json!({"bot": tokio::task::spawn_blocking(move || hub.create_group_session(&id, selection)).await??})
        }
        "createStageGroup" => {
            // 用户发送前只返回草稿，连接器凭据由 Host 既有凭据库保管。
            hub.create_stage_group(b).await?
        }
        "stageDetails" | "changeStage" => {
            let room = str_arg(&b, "botId")?.to_owned();
            let hub = hub.clone();
            let editing = method == "changeStage";
            // 交接快照读写在阻塞线程完成，关闭与调度的判断仍在同一数据库事务。
            tokio::task::spawn_blocking(
                move || {
                    if editing { hub.change_stage(&room, b) } else { hub.stage_details(&room) }
                },
            )
            .await??
        }
        "stageUsage" => {
            let id = str_arg(&b, "botId")?;
            let group = hub
                .store
                .bot(id)?
                .filter(|row| !row.deleted && row.config.stage.is_some())
                .ok_or_else(|| anyhow!("stage group not found"))?
                .config;
            let hub = hub.clone();
            // 查询本机尝试及覆盖缺口，保持账户窗口读数的原有口径。
            tokio::task::spawn_blocking(move || {
                let mut usage = hub.store.stage_usage(&group.id)?;
                usage["stageCreatedAt"] = group.created_at.into();
                Ok::<_, anyhow::Error>(usage)
            })
            .await??
        }
        // Bots from another app's export file on this computer (`import`).
        "previewImport" => crate::import::preview(&b).await?,
        "importBots" => crate::import::apply(hub, &b).await?,
        "importSkill" => json!({"name": "codync-export", "text": crate::import::EXPORT_SKILL}),
        "updateBot" => {
            let hub = hub.clone();
            json!({"bot": tokio::task::spawn_blocking(move || hub.update_bot(&b)).await??})
        }
        // The roster as the user dragged it, top first.
        "reorderBots" => {
            let ids: Vec<String> = serde_json::from_value(b["ids"].clone()).context("ids is required")?;
            let hub = hub.clone();
            tokio::task::spawn_blocking(move || hub.reorder_bots(&ids)).await??;
            json!({})
        }
        // Every bot in section `from` moves to `to`; `null` or an empty name removes the section.
        "renameSection" => {
            const TO: &str = "`to` must be a section name or null";
            let from = str_arg(&b, "from")?.to_owned();
            // A missing `to` is refused, not read as null: a client bug must not strip every section.
            let to: Option<String> = serde_json::from_value(b.get("to").cloned().context(TO)?).context(TO)?;
            let hub = hub.clone();
            tokio::task::spawn_blocking(move || hub.rename_section(&from, to.as_deref())).await??;
            json!({})
        }
        // The apps name a dev server, and never send a command.
        "devServerStop" => {
            crate::dev_servers::stop(hub, str_arg(&b, "botId")?, str_arg(&b, "id")?, "user").await?;
            json!({})
        }
        "devServerRestart" => {
            crate::dev_servers::restart(hub, str_arg(&b, "botId")?, str_arg(&b, "id")?).await?;
            json!({})
        }
        "devServerRemove" => {
            crate::dev_servers::remove(hub, str_arg(&b, "botId")?, str_arg(&b, "id")?)?;
            json!({})
        }
        "devServerLogs" => {
            json!({"text": crate::dev_servers::shown_logs(hub, str_arg(&b, "botId")?, str_arg(&b, "id")?).await?})
        }
        "deleteBot" => {
            hub.delete_bot(str_arg(&b, "botId")?)?;
            json!({})
        }
        // The user makes this bot the lead, in place of the one before.
        "setLead" => {
            crate::chat::lead::set(hub, str_arg(&b, "botId")?)?;
            json!({})
        }
        "markRead" => {
            hub.mark_read(str_arg(&b, "botId")?)?;
            json!({})
        }
        // One chunk of a file for a later `send` (`attachments`); base64 `data` at `offset`.
        "upload" => {
            use base64::Engine as _;
            let row = hub.store.bot(str_arg(&b, "botId")?)?.filter(|r| !r.deleted);
            let root = crate::chat::uploads::root(&row.ok_or_else(|| anyhow!("unknown bot"))?.config);
            let (id, name) = (str_arg(&b, "uploadId")?.to_owned(), str_arg(&b, "name")?.to_owned());
            let data =
                base64::engine::general_purpose::STANDARD.decode(str_arg(&b, "data")?).context("invalid data")?;
            let (offset, done) = (b["offset"].as_u64().unwrap_or(0), b["done"] == true);
            let attachment = tokio::task::spawn_blocking(move || {
                crate::chat::uploads::append(&root, &id, &name, offset, &data, done)
            })
            .await??;
            json!({"attachment": attachment})
        }
        // A sent file, back in chunks (chat previews on other devices).
        "readUpload" => {
            use base64::Engine as _;
            let row = hub.store.bot(str_arg(&b, "botId")?)?.filter(|r| !r.deleted);
            let root = crate::chat::uploads::root(&row.ok_or_else(|| anyhow!("unknown bot"))?.config);
            let id = str_arg(&b, "uploadId")?.to_owned();
            let offset = b["offset"].as_u64().unwrap_or(0);
            let (data, size) =
                tokio::task::spawn_blocking(move || crate::chat::uploads::read(&root, &id, offset, 384 * 1024))
                    .await??;
            json!({"data": base64::engine::general_purpose::STANDARD.encode(data), "size": size})
        }
        "send" => {
            let bot = str_arg(&b, "botId")?;
            let raw_text = b["text"].as_str().unwrap_or_default();
            let text = raw_text.trim();
            let uploads: Vec<&str> =
                b["attachments"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if text.is_empty() && uploads.is_empty() {
                bail!("empty message");
            }
            let nonce = b["clientNonce"].as_str().unwrap_or_default();
            if !nonce.is_empty()
                && let Some(e) = hub.store.find_by_nonce(bot, nonce)
            {
                return Ok(ControlFlow::Break(json!({"entry": e})));
            }
            let row = hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            // 在原始请求上校验 UTF-16 范围，host 裁剪正文时同步移动绑定。
            let supplied: Vec<crate::chat::mentions::Mention> =
                b.get("mentions").cloned().map(serde_json::from_value).transpose()?.unwrap_or_default();
            let mentions =
                crate::chat::mentions::checked(raw_text, &supplied, &row.config.members).inspect_err(|_| {
                    // 只上报失败分类，不记录正文、成员ID或名称。
                    crate::analytics::capture(
                        hub,
                        crate::analytics::Event::GroupMentionRejected,
                        json!({"category":"invalid_binding"}).as_object().cloned().unwrap_or_default(),
                    );
                })?;
            // `replyTo`: the message of this chat it answers (`chat::replies`).
            let reply = match b["replyTo"].as_str().filter(|id| !id.is_empty()) {
                Some(id) => Some(crate::chat::replies::quote(&hub.store, bot, id)?),
                None => None,
            };
            let mut attachments = Vec::new();
            if !uploads.is_empty() {
                let root = crate::chat::uploads::root(&row.config);
                for id in uploads {
                    attachments.push(crate::chat::uploads::resolve(&root, id)?.1);
                }
            }
            let mut data =
                json!({"text": text, "clientNonce": nonce, "attachments": attachments, "mentions": mentions});
            // What the user pasted rather than typed: memory never takes it as the user's own words.
            let pasted: Vec<&str> = b["pasted"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|p| !p.is_empty() && text.contains(p))
                .collect();
            if !pasted.is_empty() {
                data["pasted"] = json!(pasted);
            }
            if let Some(reply) = reply {
                data["replyTo"] = reply;
            }
            // Said on a voice call (on-device or speech to text): the apps show it in the call's conversation.
            if let Some(call) = b["call"].as_str() {
                data["call"] = crate::chat::calls::checked(call)?.into();
                crate::chat::calls::heard(hub, bot, call, caller.device_key())?;
            }
            json!({"entry": crate::chat::messages::post(hub, &row.config, data)?})
        }
        // A voice call is on (its audio flows): the lead speaks first.
        "callStarted" => {
            let call = b["callId"].as_str().map(crate::chat::calls::checked).transpose()?;
            crate::chat::lead::call_started(hub, str_arg(&b, "botId")?, call)?;
            json!({})
        }
        // A finished voice call from an app older than `endCall`, as a line in the chat ("Voice chat · 0:16").
        "logCall" => {
            let bot = str_arg(&b, "botId")?;
            hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            let seconds = b["seconds"].as_u64().unwrap_or(0);
            let data = json!({"text": crate::chat::calls::UNTITLED, "callSeconds": seconds});
            let e = hub.add_entry(&Lane::main(bot), EntryKind::Notice, hub.store.max_turn(bot), &data);
            json!({"entry": e})
        }
        // A voice call, owned by the host (`chat::calls`); only the device that starts it hears what its voice should
        // say.
        "startCall" => crate::chat::calls::start(hub, &b, caller.device_key()).await?,
        // `lines`: what was said on the call that the app hadn't sent yet, which the tool may hand on.
        "callTool" => {
            let call = crate::chat::calls::checked(str_arg(&b, "callId")?)?;
            let result = crate::chat::calls::tools::call(hub, call, str_arg(&b, "name")?, &b["arguments"], &b["lines"]);
            json!({"result": result.await})
        }
        "callLines" => {
            crate::chat::calls::add_lines(hub, crate::chat::calls::checked(str_arg(&b, "callId")?)?, &b["lines"])?;
            json!({})
        }
        // What a call carried, for its conversation: the chat an app holds may not reach back that far.
        "callEntries" => {
            let bot = str_arg(&b, "botId")?;
            let call = crate::chat::calls::checked(str_arg(&b, "callId")?)?;
            json!({"entries": hub.store.call_entries(bot, call, crate::chat::calls::CALL_ENTRIES)?})
        }
        "endCall" => {
            let call = crate::chat::calls::checked(str_arg(&b, "callId")?)?;
            let seconds = b["seconds"].as_u64().unwrap_or(0);
            json!({"entry": crate::chat::calls::end(hub, str_arg(&b, "botId")?, call, seconds, &b["lines"])?})
        }
        "stop" => {
            let bot = str_arg(&b, "botId")?;
            match hub.store.bot(bot)? {
                Some(row) if row.config.is_group() => crate::chat::group::stop(hub, bot)?,
                _ => hub.send_cmd(bot, Cmd::Stop)?,
            }
            json!({})
        }
        // A message of the user's waiting for its turn: handled now (`steer`) or taken back (`unqueue`).
        "steer" | "unqueue" => {
            let (bot, entry_id) = (str_arg(&b, "botId")?, str_arg(&b, "entryId")?.to_owned());
            let queued = hub
                .store
                .entry(&entry_id)
                .filter(|e| e.bot_id == bot && e.kind == EntryKind::User.as_str() && e.data["status"] == "queued");
            if queued.is_none() {
                bail!("that message isn't waiting any more");
            }
            let cmd = if method == "steer" { Cmd::Steer { entry_id } } else { Cmd::Unqueue { entry_id } };
            hub.send_cmd(bot, cmd)?;
            json!({})
        }
        "switchBack" => {
            hub.send_cmd(str_arg(&b, "botId")?, Cmd::SwitchBack)?;
            json!({})
        }
        "retryDeliverySave" => {
            hub.send_cmd(str_arg(&b, "botId")?, Cmd::RetryDeliverySave)?;
            json!({})
        }
        "retryDelivery" => {
            let delivery = hub.retry_delivery(str_arg(&b, "deliveryId")?)?;
            crate::chat::group::ensure_runner(hub, &delivery.room_id);
            json!({"delivery":delivery})
        }
        "newSession" => {
            let mode = b["mode"].as_str().unwrap_or("after");
            anyhow::ensure!(["after", "immediate", "cancel"].contains(&mode), "invalid session switch mode");
            hub.send_cmd(str_arg(&b, "botId")?, Cmd::SessionSwitch { mode: mode.into() })?;
            json!({})
        }
        "memory" | "memoryFile" => crate::chat::memory::call(hub, method, &b).await?,
        "library" => crate::chat::library::list(hub, str_arg(&b, "botId")?)?,
        "readArtifact" | "renameArtifact" => crate::chat::artifacts::api(hub, method, &b).await?,
        "editHandoff" | "keepConversation" => crate::chat::handoff::call(hub, method, &b)?,
        "react" => {
            let e = hub.react(str_arg(&b, "entryId")?, str_arg(&b, "emoji")?)?;
            json!({"entry": e})
        }
        "respondPermission" => {
            let entry_id = str_arg(&b, "entryId")?;
            let e = hub.store.entry(entry_id).ok_or_else(|| anyhow!("unknown card"))?;
            let option_id = b["optionId"].as_str().map(str::to_owned);
            // A card in a group belongs to the bot that asked.
            let bot = e.data["author"].as_str().unwrap_or(&e.bot_id);
            hub.send_cmd(bot, Cmd::Permission { entry_id: entry_id.to_owned(), option_id })?;
            json!({})
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
