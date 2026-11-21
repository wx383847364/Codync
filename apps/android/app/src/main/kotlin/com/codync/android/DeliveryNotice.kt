package com.codync.android

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.*

/** 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。 */
@Composable internal fun DeliveryNotice(entry: Entry, state: AppState, store: AppStore) {
    val rows = entry.data["deliveryRows"]?.let { WireJson.decodeFromJsonElement(ListSerializer(DeliveryRow.serializer()), it) }.orEmpty()
    var expanded by remember(entry.id) { mutableStateOf(false) }

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
            Text(DeliveryRow.summary(rows), style = Type.footnote, color = Palette.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }) {
            Text("第 ${entry.data["deliveryAttempt"]?.jsonPrimitive?.intOrNull ?: 1} 次投递" + (if (entry.data.text("parentDeliveryId") != null) " · 重试投递" else ""), style = Type.footnote, color = Palette.secondary)
            for (row in rows) Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
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
}
