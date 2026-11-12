package com.codync.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import java.util.UUID
import java.util.Date
import java.text.DateFormat
import java.text.NumberFormat
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/** 确认阶段目标和角色，完成后保留首条草稿供用户发送。 */
@Composable internal fun StageGroupEditor(group: Bot, state: AppState, store: AppStore) {
    val dismiss = LocalDismissModal.current
    val requestId = remember(group.id) { UUID.randomUUID().toString() }
    var name by remember { mutableStateOf("") }
    var goal by remember { mutableStateOf("") }
    var acceptance by remember { mutableStateOf("") }
    var leader by remember { mutableStateOf("") }
    var roles by remember { mutableStateOf(emptyMap<String, StageGroupDraft.Role>()) }
    var handoffVersion by remember { mutableStateOf<String?>(null) }
    val saving = "stage-group" in state.busy

    Column(Modifier.fillMaxSize().background(Palette.background)) {
        ModalHeader("Create next stage group")
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp)) {
            FieldBox(name, { name = it }, "Stage name", fill = Palette.surface)
            PlainTextField(goal, { goal = it }, Modifier.fillMaxWidth(), "Stage goal", maxLines = 5)
            PlainTextField(acceptance, { acceptance = it }, Modifier.fillMaxWidth(), "Acceptance criteria", maxLines = 5)
            if (group.stage != null) {
                val handoff = group.stage?.handoff?.takeIf { it.acceptedAt != null }
                if (handoff == null) {
                    Text(tr("Review and accept a handoff in Stage details first."), style = Type.footnote, color = Palette.secondary)
                } else {
                    CodyncSwitch(handoffVersion == handoff.file.id, { on -> handoffVersion = handoff.file.id.takeIf { on } }, enabled = !saving) {
                        Text(tr("Use accepted handoff") + " · " + handoff.file.sha256.take(12), style = Type.body)
                    }
                }
            }
            Text(tr("New bots start with their own workspace and Codync shared memory off."), style = Type.footnote, color = Palette.secondary)
            Text(tr("Keep only stable instructions for the selected roles."), style = Type.footnote, color = Palette.secondary)
            for (bot in group.memberBots(state.bots)) {
                val role = roles[bot.id]
                // 取消角色时清理负责人，要求用户重新选择。
                CodyncSwitch(role != null, { on ->
                    if (!on && leader == bot.id) leader = ""
                    roles = if (on) roles + (bot.id to StageGroupDraft.Role(bot.id, bot.rev, bot.description)) else roles - bot.id
                }, enabled = !saving) { Text(bot.name, style = Type.body) }
                if (role != null) {
                    Text(listOfNotNull(bot.backend, bot.model).joinToString(" · "), style = Type.caption, color = Palette.secondary)
                    PlainTextField(role.description, { roles = roles + (bot.id to role.copy(description = it)) },
                        Modifier.fillMaxWidth(), "Role instructions", maxLines = 6)
                    if (bot.cwd.isNotEmpty() && !bot.managedWorkspace) {
                        CodyncSwitch(role.reuseProject, { reuse -> roles = roles + (bot.id to role.copy(reuseProject = reuse)) },
                            enabled = !saving) { Text(tr("Reuse project folder"), style = Type.body) }
                        Text(bot.cwd, style = Type.caption, color = Palette.secondary)
                    }
                    if (bot.stageMemoryShared == true) {
                        Text(tr("This agent shares its own memory across sessions."), style = Type.caption, color = Palette.secondary)
                    }
                    LinkButton(if (leader == bot.id) "Stage lead" else "Set as stage lead", enabled = !saving && leader != bot.id) {
                        leader = bot.id
                    }
                }
            }
            state.error?.let { Text(tr(it), style = Type.footnote, color = Palette.danger) }
            PrimaryButton(if (saving) "Creating…" else "Create stage group", enabled = !saving && leader.isNotEmpty() &&
                name.isNotBlank() && goal.isNotBlank() && acceptance.isNotBlank() && (group.stage == null || handoffVersion != null)) {
                // 固定来源成员顺序和请求 ID，保留失败后的安全重试。
                val draft = StageGroupDraft(group.id, requestId, name, goal, acceptance, leader,
                    group.members.mapNotNull(roles::get), handoffVersion)
                store.createStageGroup(draft) { dismiss() }
            }
        }
    }
}

/** 人工编辑交接并处理验收、关闭和重开，历史交接在关闭后可继续读取。 */
@Composable internal fun StageDetailsEditor(botId: String, state: AppState, store: AppStore) {
    val dismiss = LocalDismissModal.current
    val scope = rememberCoroutineScope()
    var details by remember(botId) { mutableStateOf<StageDetails?>(null) }
    var markdown by remember(botId) { mutableStateOf("") }
    var evidence by remember(botId) { mutableStateOf("") }
    var baseVersion by remember(botId) { mutableStateOf<String?>(null) }
    var error by remember(botId) { mutableStateOf<String?>(null) }
    var incomingOpen by remember(botId) { mutableStateOf(false) }
    var busy by remember(botId) { mutableStateOf(false) }

    /** 读取失败时保留编辑状态，保存成功才用已确认版本刷新输入框。 */
    suspend fun load(change: StageChange? = null) {
        busy = true
        error = null
        try {
            val result = store.stageDetails(botId, change)
            if (details == null || change?.action == "saveHandoff") {
                markdown = result.markdown ?: result.template
                evidence = result.bot.stage?.handoff?.evidence.orEmpty()
                baseVersion = result.bot.stage?.handoff?.file?.id
            }
            details = result
        } catch (failure: CancellationException) { throw failure }
        catch (failure: Exception) { error = failure.message }
        finally { busy = false }
    }
    LaunchedEffect(botId) { load() }

    Column(Modifier.fillMaxSize().background(Palette.background)) {
        ModalHeader("Stage details")
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            val current = details
            val stage = current?.bot?.stage
            if (current != null && stage != null) {
                val closed = stage.closedAt != null
                val handoff = stage.handoff
                val pending = current.work.pending.isNotEmpty()
                val changed = markdown.trim() != current.markdown.orEmpty() || evidence.trim() != handoff?.evidence.orEmpty()
                Text(current.bot.name + " · " + tr(if (closed) "Stage closed" else "Stage open"), style = Type.headline)
                Text(tr(if (handoff?.acceptedAt == null) "Stage awaiting acceptance" else "Stage accepted"), style = Type.footnote)
                for ((status, count) in current.work.counts) Text(tr(status) + " · " + count, style = Type.footnote)
                for (item in current.work.pending) Text(item.name + " · " + tr(item.status), style = Type.footnote)
                if (current.incomingMarkdown != null) {
                    SecondaryButton("Incoming handoff") { incomingOpen = !incomingOpen }
                    if (incomingOpen) Text(current.incomingMarkdown.orEmpty(), style = Type.body)
                }
                if (current.handoffUnavailable || current.incomingUnavailable)
                    Text(tr("The handoff file is unavailable."), style = Type.footnote, color = Palette.danger)
                handoff?.let { Text(it.file.name + " · " + it.file.sha256.take(12), style = Type.caption) }
                if (closed) {
                    current.markdown?.let { Text(it, style = Type.body) }
                    handoff?.evidence?.takeIf { it.isNotEmpty() }?.let { Text(it, style = Type.body) }
                } else {
                    PlainTextField(markdown, { markdown = it }, Modifier.fillMaxWidth(), "Handoff Markdown", maxLines = 16, enabled = !busy)
                    PlainTextField(evidence, { evidence = it }, Modifier.fillMaxWidth(), "Durable evidence references", maxLines = 5, enabled = !busy)
                    if (current.sharedFiles.isNotEmpty()) Text(tr("Shared evidence files"), style = Type.footnote)
                    for (file in current.sharedFiles) {
                        val reference = "file:" + file.id
                        SecondaryButton(action = {
                            // 保存用户点选的固定快照，阶段创建复制时检查哈希。
                            evidence = listOf(evidence.trimEnd(), reference).filter(String::isNotEmpty).joinToString("\n")
                        }, enabled = !busy && reference !in evidence.lines(), label = { Text(file.name + " · " + file.sha256.take(12)) })
                    }
                    SecondaryButton("Save handoff", enabled = !busy && changed && markdown.isNotBlank()) {
                        scope.launch { load(StageChange(botId, "saveHandoff", markdown, evidence, baseVersion)) }
                    }
                }
                if (handoff != null) SecondaryButton("Accept stage", enabled = !busy && !changed && handoff.evidence.isNotBlank() &&
                    baseVersion == handoff.file.id && handoff.acceptedAt == null && !pending && !current.handoffUnavailable) {
                    scope.launch { load(StageChange(botId, "accept", version = handoff.file.id)) }
                }
                SecondaryButton(if (closed) "Reopen stage" else "Close stage", enabled = !busy && (closed || !pending)) {
                    scope.launch { load(StageChange(botId, if (closed) "reopen" else "close")) }
                }
                if (!closed && current.bot.lastMessage == null && state.drafts[botId]?.text.isNullOrEmpty()) {
                    SecondaryButton("Restore first draft") {
                        store.editDraft(botId, current.initialDraft)
                        store.openBot(botId)
                        dismiss()
                    }
                }
            }
            SecondaryButton("Refresh", enabled = !busy) { scope.launch { load() } }
            error?.let { Text(tr(it), style = Type.footnote, color = Palette.danger) }
            if (details != null) StageUsageSection(botId, store)
        }
    }
}

/** 阶段统计按 Host 汇总字段展示，空值和已知零值有不同显示。 */
@Composable private fun StageUsageSection(botId: String, store: AppStore) {
    val scope = rememberCoroutineScope()
    var usage by remember(botId) { mutableStateOf<StageUsage?>(null) }
    var busy by remember(botId) { mutableStateOf(false) }
    var error by remember(botId) { mutableStateOf<String?>(null) }
    SecondaryButton(if (usage == null) "Stage usage" else "Refresh stage usage", enabled = !busy) {
        // 按需查询，保留之前的可用统计直到新响应返回。
        scope.launch {
            busy = true
            error = null
            try { usage = store.stageUsage(botId) }
            catch (failure: CancellationException) { throw failure }
            catch (failure: Exception) { error = failure.message }
            finally { busy = false }
        }
    }
    error?.let { Text(tr(it), style = Type.footnote, color = Palette.danger) }
    val current = usage ?: return
    val date = DateFormat.getDateInstance().format(Date(current.observedSince))
    val number = NumberFormat.getIntegerInstance()
    Text(tr("Recording since {date}").replace("{date}", date), style = Type.footnote)
    if (current.stageCreatedAt < current.observedSince) Text(tr("Earlier attempts may be missing."), style = Type.footnote)
    Text(tr("Reported fields may overlap. Account usage is separate."), style = Type.footnote)
    for (section in current.sections) {
        val model = section.model ?: tr("Unknown model")
        val subject = section.backend?.let { " · $it · $model" }.orEmpty()
        Text(tr(section.label) + subject, style = Type.headline)
        for (row in section.rows) {
            val value = row.value?.let(number::format) ?: tr("Unavailable")
            val coverage = row.reports?.let { " ($it/${row.attempts ?: 0})" }.orEmpty()
            Text(tr(row.label) + " · " + value + coverage, style = Type.footnote)
        }
    }
}
