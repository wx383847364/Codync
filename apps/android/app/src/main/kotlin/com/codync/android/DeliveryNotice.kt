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
