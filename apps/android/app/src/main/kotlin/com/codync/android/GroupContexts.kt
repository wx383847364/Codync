package com.codync.android

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*
import java.text.NumberFormat

/** 精确定位从群聊进入的私聊交接。 */
data class PrivateGroupHandoffTarget(val botId: String, val entryId: String)

private data class GroupReview(val botId: String, val deliveryId: String?, val revision: String, val text: String)

/** 固定档位和实际用量共用十进制单位，详情保留精确整数。 */
internal fun groupContextCompact(value: Long): String = if (value == 1_000_000L) "1M" else
    NumberFormat.getNumberInstance().apply { maximumFractionDigits = 1; isGroupingUsed = false }.format(value / 1000.0) + "K"

/** 进度文本不截断超过 100% 的真实占比。 */
internal fun groupContextPercent(used: Long?, size: Long): String? = used?.takeIf { size > 0 }?.let {
    NumberFormat.getPercentInstance().apply { minimumFractionDigits = 1; maximumFractionDigits = 1 }.format(it.toDouble() / size)
}

/** 群顶部固定成员卡，交接操作与显示基准属于当前房间。 */
@Composable internal fun GroupContexts(room: Bot, state: AppState, store: AppStore, snapshot: GroupSessionContexts?,
    update: (GroupSessionContexts) -> Unit, inspecting: String?, inspect: (String?) -> Unit, modifier: Modifier = Modifier) {
    val scope = rememberCoroutineScope()
    val members = room.memberBots(state.bots)
    val online = state.actualConnection is LinkState.Ready && state.mismatch == null
    val basis = snapshot?.basis ?: 512_000L
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var refreshError by remember { mutableStateOf<String?>(null) }
    var review by remember { mutableStateOf<GroupReview?>(null) }
    val memberRevisions = members.map { it.id to it.rev }
    LaunchedEffect(room.id, room.rev, memberRevisions, online) {
        // 合并同一回合的多个更新，切换房间会取消旧快照请求。
        if (!online) return@LaunchedEffect
        delay(90)
        try {
            update(WireJson.decodeFromJsonElement(store.hostCall("groupSessionContexts", body("roomId" to room.id))))
            refreshError = null
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { refreshError = failure.localizedMessage }
    }

    Column(modifier.fillMaxWidth().background(Palette.background).padding(horizontal = 12.dp, vertical = 5.dp),
        verticalArrangement = Arrangement.spacedBy(5.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            Column(Modifier.weight(1f)) {
                Text(tr("Member context"), style = Type.footnote.medium, color = Palette.text)
                if (snapshot?.members?.firstOrNull()?.shared == true) {
                    Text(tr("Shares private sessions"), style = Type.caption, color = Palette.secondary)
                }
            }
            Text(tr("Display basis"), style = Type.caption, color = Palette.secondary)
            for (value in listOf(256_000L, 512_000L, 1_000_000L)) {
                Text(groupContextCompact(value), Modifier.clip(RoundedCornerShape(6.dp))
                    .background(if (value == basis) Palette.bubbleAgent else Palette.background)
                    .semantics { selected = value == basis }.clickable(enabled = online && !busy) {
                        // 群内只保存一份档位，覆盖时以 Host 响应为准。
                        scope.launch {
                            busy = true
                            error = null
                            try {
                                val request = buildJsonObject { put("roomId", room.id); put("basis", value) }
                                update(WireJson.decodeFromJsonElement(store.hostCall("setGroupContextBasis", request)))
                            } catch (cancelled: CancellationException) { throw cancelled }
                            catch (failure: Exception) { error = failure.localizedMessage }
                            finally { busy = false }
                        }
                    }.padding(horizontal = 6.dp, vertical = 5.dp), style = Type.caption.medium, color = Palette.text)
            }
        }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            for (bot in members) {
                val member = snapshot?.members?.firstOrNull { it.botId == bot.id }
                GroupContextCard(bot, member, basis, busy || !online, { inspect(bot.id) }) {
                    // 阻塞派工保留原身份，共享摘要在对应的私聊展示。
                    scope.launch {
                        busy = true
                        error = null
                        try {
                            val response = WireJson.decodeFromJsonElement<GroupSessionContexts.Review>(store.hostCall(
                                "prepareGroupMemberSession", body("roomId" to room.id, "botId" to bot.id)))
                            val entryId = response.privateEntryId
                            val deliveryId = response.deliveryId
                            if (entryId != null) {
                                store.openBot(bot.id, entryId)
                            } else if (response.phase !in listOf("approved", "switched")) {
                                val draft = if (deliveryId != null) WireJson.decodeFromJsonElement<GroupSessionContexts.Review>(
                                    store.hostCall("groupHandoff", body("deliveryId" to deliveryId))) else response
                                if (draft.text != null && draft.revision != null) review = GroupReview(
                                    bot.id, deliveryId, requireNotNull(draft.revision), requireNotNull(draft.text))
                            }
                        } catch (cancelled: CancellationException) { throw cancelled }
                        catch (failure: Exception) { error = failure.localizedMessage }
                        finally { busy = false }
                    }
                }
            }
        }
        if (review == null) (error ?: refreshError)?.let { Text(it, style = Type.caption, color = Palette.danger) }
    }
    CodyncSheet(inspecting != null, { inspect(null) }) {
        val member = snapshot?.members?.firstOrNull { it.botId == inspecting }
        if (member != null) Column {
            ModalHeader(members.firstOrNull { it.id == inspecting }?.name ?: "Member context")
            CardForm {
                CardSection("Current context") {
                    row { Text(member.used?.let { "${NumberFormat.getIntegerInstance().format(it)} token" }
                        ?: tr(if (member.hasSession) "Usage not reported" else "No session yet")) }
                    row { Text("${tr("Display basis")} · ${groupContextCompact(basis)} · ${groupContextPercent(member.used, basis) ?: "—"}") }
                    row { Text("${tr("Actual model window")} · ${member.size?.let(::groupContextCompact) ?: "—"} · ${member.size?.let { groupContextPercent(member.used, it) } ?: "—"}") }
                    if (member.shared) row { Text(tr("This also starts a new private session")) }
                }
            }
        }
    }
    CodyncSheet(review != null, { review = null }) {
        val draft = review
        if (draft != null) Column {
            ModalHeader("Open new session")
            CardForm {
                CardSection("Summary for the new session", footer = "The new session starts after the current turn.") {
                    row {
                        BasicTextField(draft.text, { review = draft.copy(text = it) }, Modifier.fillMaxWidth(), minLines = 7,
                            textStyle = Type.footnote.copy(color = Palette.text), cursorBrush = SolidColor(Palette.accent))
                    }
                }
                error?.let { Text(it, style = Type.caption, color = Palette.danger) }
                PrimaryButton("Confirm new session", enabled = !busy && draft.text.isNotBlank()) {
                    // 同一次确认会保存文本和任务身份，失败后保留编辑。
                    scope.launch {
                        busy = true
                        try {
                            val method = if (draft.deliveryId != null) "confirmGroupHandoff" else "confirmGroupMemberSession"
                            val target = if (draft.deliveryId != null) body("deliveryId" to draft.deliveryId)
                                else body("roomId" to room.id, "botId" to draft.botId)
                            store.hostCall(method, JsonObject(target + body("revision" to draft.revision, "text" to draft.text)))
                            review = null
                        } catch (cancelled: CancellationException) { throw cancelled }
                        catch (failure: Exception) { error = failure.localizedMessage }
                        finally { busy = false }
                    }
                }
            }
        }
    }
}

/** 成员卡将新开入口放在右上角，读数区域打开详情。 */
@Composable private fun GroupContextCard(bot: Bot, member: GroupSessionContexts.Member?, basis: Long,
    disabled: Boolean, inspect: () -> Unit, open: () -> Unit) {
    Column(Modifier.width(197.dp).clip(RoundedCornerShape(10.dp)).background(Palette.bubbleAgent).padding(7.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
            CharacterAvatar(bot.avatarShape, bot.avatarColor, 18.dp, customImage = bot.customIcon?.dataUrl)
            Text(bot.name, Modifier.weight(1f), style = Type.caption.medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (member?.handoff?.phase in listOf("approved", "switched")) {
                Text(tr("New session after this turn"), style = Type.caption, color = Palette.secondary)
            } else if (member?.recommended == true || member?.handoff?.phase == "draft") {
                Text(tr("Open new session"), Modifier.clip(RoundedCornerShape(6.dp))
                    .clickable(enabled = !disabled, onClick = open).padding(3.dp), style = Type.caption.medium, color = Palette.accent)
            }
        }
        Column(Modifier.clickable(enabled = member != null, onClick = inspect), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(member?.used?.let(::groupContextCompact) ?: "—", Modifier.weight(1f), style = Type.body.medium)
                Text(groupContextPercent(member?.used, basis) ?: tr(if (member?.hasSession == false) "No session yet" else "Usage not reported"),
                    style = Type.caption, color = Palette.secondary)
            }
            Box(Modifier.fillMaxWidth().height(3.dp).clip(RoundedCornerShape(4.dp)).background(Palette.secondary.copy(alpha = 0.18f))) {
                Box(Modifier.fillMaxWidth(((member?.used ?: 0).toFloat() / basis).coerceIn(0f, 1f)).height(3.dp)
                    .background(AvatarPalette.tint(bot.avatarColor)))
            }
        }
    }
}

/** 私聊页精准展示由成员卡打开的交接，和消息里的卡共用编辑与确认操作。 */
@Composable internal fun PrivateGroupHandoff(bot: Bot, state: AppState, store: AppStore) {
    val target = state.privateGroupHandoff?.takeIf { it.botId == bot.id }
    var entry by remember(bot.id) { mutableStateOf<Entry?>(null) }
    LaunchedEffect(target) {
        if (target == null) return@LaunchedEffect
        try {
            val result = store.hostCall("privateGroupHandoff", body("botId" to bot.id, "entryId" to target.entryId)).jsonObject
            entry = WireJson.decodeFromJsonElement(result.getValue("entry"))
            store.dismissPrivateGroupHandoff()
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { store.showError(failure); store.dismissPrivateGroupHandoff() }
    }
    CodyncSheet(entry != null, { entry = null }) {
        val latest = entry?.let { saved -> state.entries.firstOrNull { it.id == saved.id } ?: saved }
        Column {
            ModalHeader("Open new session")
            (latest?.data?.get("handoff") as? JsonObject)?.let { HandoffCardRow(requireNotNull(latest), it, state, store) }
        }
    }
}
