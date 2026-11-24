package com.codync.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.graphics.SolidColor
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.*

/** 暂停后保留任务与摘要，用户仍能在原群继续。 */
@Composable internal fun HeldDeliveryRow(chat: Bot, task: GroupDeliveryControl, state: AppState, store: AppStore) {
    val paused = task.status == "paused"
    val status = tr(if (paused) "Paused" else "Continuing…")
    val name = state.bots.firstOrNull { it.id == task.targetBotId }?.name ?: tr("Bot")
    val preview = task.preview.takeIf { it.isNotEmpty() }?.let { " · $it" }.orEmpty()
    val stage = chat.stage
    Row(Modifier.background(Palette.bubbleAgent, RoundedCornerShape(18.dp))
        .padding(horizontal = 16.dp, vertical = if (paused) 4.dp else 12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("$name · $status$preview", Modifier.weight(1f, fill = false), style = Type.subheadline,
            color = Palette.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (paused) {
            // 继续同一投递，阶段关闭时等重新开启。
            IconButton("Continue", "play.fill", enabled = stage?.closedAt == null && stage?.archivedAt == null,
                action = { store.setDeliveryPaused(chat.id, task, paused = false) })
        }
    }
}

/** 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。 */
@Composable internal fun DeliveryNotice(entry: Entry, state: AppState, store: AppStore, deliveryId: String? = null, openEntry: ((String) -> Unit)? = LocalChatEntryJump.current, expandedRequest: String? = null) {
    val rows = entry.data["deliveryRows"]?.let { WireJson.decodeFromJsonElement(ListSerializer(DeliveryRow.serializer()), it) }.orEmpty()
        .filter { deliveryId == null || it.deliveryId == deliveryId }
    var expanded by remember(entry.id, deliveryId) { mutableStateOf(deliveryId != null) }
    LaunchedEffect(expandedRequest) {
        // 来自成员页的定位让原卡片展开。
        if (expandedRequest != null) expanded = true
    }

    val hasOverview = state.hello?.get("groupSessionContexts")?.jsonPrimitive?.booleanOrNull == true
    // 成员卡负责新开会话时，原派工默认只占一行。
    val phase = if (hasOverview) listOf("draft", "approved", "switched", "sent")
        .firstOrNull { phase -> rows.any { it.groupHandoff?.phase == phase } } else null
    val summary = when (phase) {
        "draft" -> tr("Waiting for a new session")
        "sent" -> tr("New session started. Original dispatch resumed.")
        "approved", "switched" -> tr("Preparing a new session")
        else -> DeliveryRow.summary(rows)
    }
    // 只显示同群中可接收引导的回合，提交时仍由 host 复核。
    fun activeId(row: DeliveryRow): String? {
        if (!row.canGuide || row.status != "queued" || state.connection !is LinkState.Ready) return null
        val member = state.bots.firstOrNull { it.id == row.targetBotId }
        return member?.takeIf { it.status == "working" && it.storageError == null && it.workingChat == entry.botId }?.guidingDeliveryId
    }

    // 绑定渲染时的回合身份，点击迟到时由 host 拒绝旧操作。
    val eligible = rows.mapNotNull { row -> activeId(row)?.let { row.deliveryId to it } }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        // 展开和收起复用通用动画与可访问性语义。
        Disclosure(expanded, { expanded = !expanded }, Modifier.weight(1f), {
            Text(summary, style = Type.footnote, color = Palette.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }) {
            Text("第 ${entry.data["deliveryAttempt"]?.jsonPrimitive?.intOrNull ?: 1} 次投递" + (if (entry.data.text("parentDeliveryId") != null) " · 重试投递" else ""), style = Type.footnote, color = Palette.secondary)
            for (row in rows) Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("${state.bots.firstOrNull { it.id == row.targetBotId }?.name ?: "已移除成员"}：${row.statusText}" + (if (row.detail.isEmpty()) "" else " · ${row.detail}"),
                    modifier = Modifier.weight(1f), style = Type.footnote, color = Palette.secondary)
                val id = activeId(row)
                if (id != null) {
                    // 每个操作只引导该行对应的成员。
                    SecondaryButton("引导", enabled = "guide:" + row.deliveryId !in state.busy,
                        action = { store.guideDelivery(row.deliveryId, id) })
                } else if (row.status in listOf("failed", "interrupted", "noReply")) {
                    SecondaryButton("重新尝试", action = { store.command("retryDelivery", body("deliveryId" to row.deliveryId)) })
                }
                }
                DeliveryActivity(row, entry.botId, store, openEntry?.takeIf {
                    state.entries.any { it.id == row.resultId }
                })
            }
        }
        if (!expanded && eligible.isNotEmpty()) {
            val (deliveryId, activeId) = eligible.first()
            SecondaryButton(if (eligible.size == 1) "引导" else "引导…",
                enabled = eligible.size > 1 || "guide:" + deliveryId !in state.busy, action = {
                    // 多位成员可以接收引导时，先展开列表明确选择目标。
                    if (eligible.size == 1) store.guideDelivery(deliveryId, activeId) else expanded = true
                })
        }
    }
    if (!hasOverview) for (row in rows) row.groupHandoff?.let { GroupDeliveryHandoffRow(row, it, state, store) }

}

/** 终态保留步骤时间，结果从同群历史读取。 */
@Composable private fun DeliveryActivity(row: DeliveryRow, roomId: String, store: AppStore, openEntry: ((String) -> Unit)?) {
    val scope = rememberCoroutineScope()
    var result by remember(row.deliveryId) { mutableStateOf<Entry?>(null) }
    var error by remember(row.deliveryId) { mutableStateOf<String?>(null) }
    var loading by remember(row.deliveryId) { mutableStateOf(false) }

    /** 与聊天一样按设备时区显示时间。 */
    fun time(at: Long): String = java.text.DateFormat.getTimeInstance(java.text.DateFormat.SHORT).format(java.util.Date(at))

    row.startedAt?.let { Text("${time(it)} · ${tr("Started assignment")}", style = Type.footnote, color = Palette.secondary) }
    for (step in row.activity.orEmpty()) {
        val status = when {
            step.status == "completed" -> "Step completed"
            step.status == "failed" -> "Step failed"
            step.status == "interrupted" -> "Step interrupted"
            row.status == "started" -> "In progress"
            else -> "Step started"
        }
        Text("${time(step.at)} · ${tr(step.label)} · ${tr(status)}", style = Type.footnote,
            color = if (status == "In progress") Palette.text else Palette.secondary)
    }
    row.resultId?.let { id ->
        SecondaryButton("View result", enabled = !loading) {
            if (openEntry != null) {
                openEntry(id)
                return@SecondaryButton
            }
            // 返回原始群回复，失败时保留活动列表。
            scope.launch {
                loading = true
                error = null
                try {
                    val reply = store.hostCall("groupDeliveryResult", body("roomId" to roomId, "resultId" to id)).jsonObject
                    result = reply["entry"]?.let { WireJson.decodeFromJsonElement<Entry>(it) }
                } catch (failure: CancellationException) {
                    throw failure
                } catch (failure: Exception) {
                    error = failure.localizedMessage
                } finally {
                    loading = false
                }
            }
        }
    }
    error?.let { Text(it, style = Type.footnote, color = Palette.danger) }
    CodyncSheet(result != null, { result = null }) {
        Column {
            ModalHeader("View result")
            CardForm { Text(result?.text.orEmpty()) }
        }
    }
}

/** 独立群可以编辑交接，共享会话引导到派工成员的私聊。 */
@Composable private fun GroupDeliveryHandoffRow(row: DeliveryRow, handoff: DeliveryRow.GroupHandoff, state: AppState, store: AppStore) {
    val scope = rememberCoroutineScope()
    var revision by remember(row.deliveryId) { mutableStateOf("") }
    var draft by remember(row.deliveryId) { mutableStateOf<String?>(null) }
    var error by remember(row.deliveryId) { mutableStateOf<String?>(null) }
    var busy by remember(row.deliveryId) { mutableStateOf(false) }
    val name = state.bots.firstOrNull { it.id == row.targetBotId }?.name ?: "已移除成员"
    Column(Modifier.fillMaxWidth().background(Palette.bubbleAgent, RoundedCornerShape(12.dp)).padding(10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp)) {
        val status = when (handoff.phase) {
            "draft" -> "Session switch needs confirmation"
            "sent" -> "New session started. Original dispatch resumed."
            else -> "Preparing a new session"
        }
        Text("$name · ${tr(status)}", style = Type.footnote, color = if (handoff.phase == "draft") Palette.warning else Palette.secondary)
        if (handoff.phase == "draft") {
            Text(tr("The task still won't fit after compaction. It will resume after you confirm."), style = Type.footnote)
            if (handoff.shared) Text(tr("This also switches the bot's private session."), style = Type.footnote)
            SecondaryButton(if (handoff.shared) "Review handoff in private chat" else "Review handoff and switch session", enabled = !busy) {
                // 普通群仅导航，独立群通过专用接口读取草稿。
                if (handoff.shared) store.openBot(row.targetBotId)
                else scope.launch {
                    busy = true
                    error = null
                    try {
                        val result = store.hostCall("groupHandoff", body("deliveryId" to row.deliveryId)).jsonObject
                        revision = result.text("revision").orEmpty()
                        draft = result.text("text").orEmpty()
                    }
                    catch (failure: CancellationException) { throw failure }
                    catch (failure: Exception) { error = failure.localizedMessage }
                    finally { busy = false }
                }
            }
        }
        if (draft == null) error?.let { Text(it, color = Palette.danger, style = Type.footnote) }
    }
    CodyncSheet(draft != null, { draft = null }) {
        Column {
            ModalHeader("Review handoff and switch session") {}
            CardForm {
                CardSection("Summary for the new session", footer = "This switches the bot's session in this group. Group history and pending tasks are kept.") {
                    row {
                        BasicTextField(draft.orEmpty(), { draft = it }, Modifier.fillMaxWidth(), minLines = 7,
                            textStyle = Type.footnote.copy(color = Palette.text), cursorBrush = SolidColor(Palette.accent))
                    }
                }
                error?.let { Text(it, color = Palette.danger, style = Type.footnote) }
                PrimaryButton("Confirm new session", enabled = !busy && !draft.isNullOrBlank()) {
                    // 编辑和确认共用一次请求，失败可继续编辑原文。
                    scope.launch {
                        busy = true
                        try {
                            store.hostCall("confirmGroupHandoff", body("deliveryId" to row.deliveryId, "revision" to revision, "text" to draft.orEmpty()))
                            draft = null
                        } catch (failure: CancellationException) { throw failure }
                        catch (failure: Exception) { error = failure.localizedMessage }
                        finally { busy = false }
                    }
                }
                SecondaryButton("Not now", enabled = !busy) { draft = null }
            }
        }
    }
}
