package com.codync.android

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.unit.dp
import com.codync.android.core.Entry
import com.codync.android.core.WireJson
import com.codync.android.design.*
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

@Serializable private data class OldSession(val id: String, val at: Long, val state: String, val hasMessages: Boolean, val canDelete: Boolean)
@Serializable private data class HistorySnapshot(val active: Boolean, val sessions: List<OldSession>)
@Serializable private data class HistoryMessage(val id: String, val kind: String, val at: Long, val text: String)
@Serializable private data class HistoryMessages(val messages: List<HistoryMessage>)

/** 私聊和独立群共用历史页，删除请求仅在确认后提交。 */
@Composable internal fun SessionHistory(botId: String, store: AppStore, online: Boolean, roomId: String? = null, entries: List<Entry>) {
    val scope = rememberCoroutineScope()
    val jump = LocalChatEntryJump.current
    var showing by remember(botId, roomId) { mutableStateOf(false) }
    var reading by remember(botId, roomId) { mutableStateOf<HistorySnapshot?>(null) }
    var messages by remember(botId, roomId) { mutableStateOf<List<HistoryMessage>?>(null) }
    var pending by remember(botId, roomId) { mutableStateOf<OldSession?>(null) }
    var busy by remember(botId, roomId) { mutableStateOf(false) }
    var error by remember(botId, roomId) { mutableStateOf<String?>(null) }

    /** 历史读取和删除沿用同一作用域，缺少群 ID 意味着私聊。 */
    fun args(id: String? = null): JsonObject = body("botId" to botId, "roomId" to roomId, "historyId" to id)

    /** Host 返回新会话的确认状态，并保留切换前聊天。 */
    suspend fun load() {
        try {
            reading = WireJson.decodeFromJsonElement(HistorySnapshot.serializer(), store.hostCall("sessionHistory", args()))
        } catch (failure: CancellationException) {
            throw failure
        } catch (failure: Exception) {
            error = failure.localizedMessage
        }
    }

    /** 失败不清除历史条目，下一次删除仍需重新确认。 */
    fun act(method: String, item: OldSession) {
        scope.launch {
            busy = true
            error = null
            try {
                val result = store.hostCall(method, args(item.id))
                if (method == "deleteOldSession") load()
                else messages = WireJson.decodeFromJsonElement(HistoryMessages.serializer(), result).messages
            } catch (failure: CancellationException) {
                throw failure
            } catch (failure: Exception) {
                error = failure.localizedMessage
            } finally {
                busy = false
            }
        }
    }

    SecondaryButton("Session history", enabled = online) {
        // 每次打开重新读取当前会话及删除能力。
        messages = null
        reading = null
        error = null
        showing = true
    }
    LaunchedEffect(showing, online) { if (showing && online) load() }
    CodyncSheet(showing, { if (!busy) showing = false }) {
        Column {
            val current = messages
            ModalHeader(if (current == null) "Session history" else "Chat history")
            CardForm {
                error?.let { Text(it, color = Palette.danger) }
                if (current != null) {
                    SecondaryButton("Back") { messages = null }
                    CardSection {
                        if (current.isEmpty()) row { Text(tr("No messages")) }
                        for (item in current) row {
                            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                Text("${tr(if (item.kind == "user") "You" else "Bot")} · ${historyDate(item.at)}",
                                    style = Type.footnote, color = Palette.secondary)
                                Text(item.text)
                                if (jump != null && entries.any { it.id == item.id }) {
                                    SecondaryButton("Open original message") {
                                        // 历史页关闭后回到原会话。
                                        showing = false
                                        jump(item.id)
                                    }
                                }
                            }
                        }
                    }
                } else {
                    CardSection {
                        row {
                            Column {
                                Text(tr(if (roomId == null) "Current main session" else "Current session"))
                                Text(tr(if (reading?.active == true) "In use" else "No session yet"), color = Palette.secondary)
                            }
                        }
                        for (item in reading?.sessions.orEmpty()) row {
                            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text(historyDate(item.at))
                                Text(tr(when (item.state) {
                                    "archived" -> "Archived"
                                    "retained" -> "Retained"
                                    "deleted" -> "Only chat history remains"
                                    else -> "Archive status unknown"
                                }), color = Palette.secondary)
                                if (item.hasMessages) SecondaryButton("View chat history", enabled = !busy && online) {
                                    // 打开这个会话切换时保留的聊天记录。
                                    act("sessionHistoryMessages", item)
                                }
                                if (item.canDelete) SecondaryButton("Delete old agent session", enabled = !busy && online) {
                                    // 只选择确认目标。
                                    pending = item
                                }
                            }
                        }
                        if (error == null && reading?.sessions.isNullOrEmpty()) row {
                            Text(tr(if (reading == null) "Loading…" else "No old sessions"))
                        }
                    }
                }
            }
        }
        CodyncDialog("Delete old agent session", pending != null, { pending = null }, "The original chat history will be kept.") {
            // 确认后使用当前历史条目，取消只关闭对话框。
            val item = pending
            if (item == null) emptyList() else listOf(DialogAction("Delete", destructive = true) { act("deleteOldSession", item) })
        }
    }
}

/** 使用设备的短日期和时间。 */
private fun historyDate(at: Long): String = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(at))
