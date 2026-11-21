package com.codync.android

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
        Symbol(if (paused) "pause.fill" else "clock", size = 16.dp, tint = Palette.secondary)
        Text("$name · $status$preview", Modifier.weight(1f, fill = false), style = Type.subheadline,
            color = Palette.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (paused) {
            // 继续同一投递，阶段关闭时等重新开启。
            IconButton("Continue", "play.fill", enabled = stage?.closedAt == null && stage?.archivedAt == null,
                action = { store.setDeliveryPaused(chat.id, task, paused = false) })
        }
    }
}

/** 默认一行中文状态，详情仅含投递次数、成员状态和重试，不复制来源正文。 */
@Composable internal fun DeliveryNotice(entry: Entry, state: AppState, store: AppStore) {
    val rows = entry.data["deliveryRows"]?.let { WireJson.decodeFromJsonElement(ListSerializer(DeliveryRow.serializer()), it) }.orEmpty()
    var expanded by remember(entry.id) { mutableStateOf(false) }
    // 展开和收起复用通用动画与可访问性语义，任务状态不受折叠影响。
    Disclosure(expanded, { expanded = !expanded }, Modifier.fillMaxWidth(), {
        Text(DeliveryRow.summary(rows), style = Type.footnote, color = Palette.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }) {
        Text("第 ${entry.data["deliveryAttempt"]?.jsonPrimitive?.intOrNull ?: 1} 次投递" + (if (entry.data.text("parentDeliveryId") != null) " · 重试投递" else ""), style = Type.footnote, color = Palette.secondary)
        for (row in rows) Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("${state.bots.firstOrNull { it.id == row.targetBotId }?.name ?: "已移除成员"}：${row.statusText}" + (if (row.detail.isEmpty()) "" else " · ${row.detail}"),
                modifier = Modifier.weight(1f), style = Type.footnote, color = Palette.secondary)
            if (row.status in listOf("failed", "interrupted", "noReply")) {
                // 重试由 host 新建投递，保留旧状态和结果。
                SecondaryButton("重新尝试", action = { store.command("retryDelivery", body("deliveryId" to row.deliveryId)) })
            }
        }
    }
}
