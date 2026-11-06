package com.codync.android

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.*

/** 同一来源事件和 attempt 的聚合卡；目标行读取当前名字，原正文不跟随改名。 */
@Composable internal fun DeliveryNotice(entry: Entry, state: AppState, store: AppStore) {
    val rows = entry.data["deliveryRows"]?.let { WireJson.decodeFromJsonElement(ListSerializer(DeliveryRow.serializer()), it) }.orEmpty()
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Attempt ${entry.data["deliveryAttempt"]?.jsonPrimitive?.intOrNull ?: 1}" + (entry.data.text("parentDeliveryId")?.let { " · retry of ${it.take(8)}" } ?: ""), style = Type.footnote, color = Palette.secondary)
        Text(entry.data.text("deliveryBody").orEmpty(), style = Type.footnote, color = Palette.secondary)
        for (row in rows) Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("${state.bots.firstOrNull { it.id == row.targetBotId }?.name ?: row.targetBotId}: ${row.status} ${row.detail}",
                style = Type.footnote, color = Palette.secondary)
            if (row.status in listOf("failed", "interrupted", "noReply")) {
                // 重试由 host 新建投递，保留旧状态和结果。
                SecondaryButton("Try again", action = { store.command("retryDelivery", body("deliveryId" to row.deliveryId)) })
            }
        }
    }
}
