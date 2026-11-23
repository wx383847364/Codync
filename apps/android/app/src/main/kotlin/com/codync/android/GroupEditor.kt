package com.codync.android

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.style.TextOverflow
import com.codync.android.core.Bot
import com.codync.android.core.GroupDraft
import com.codync.android.core.LinkState
import com.codync.android.design.*
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull
import java.text.NumberFormat
import kotlinx.serialization.json.jsonPrimitive

/**
 * Change a group chat's name, what it's for and who's in it (iOS `GroupEditorView`); new groups start in [NewGroupView].
 * Bots are picked like recipients: members on top, a search, and the bots not in it yet.
 * 关闭设置页后，正在准备的共享交接不得再打开私聊。
 */
@Composable internal fun GroupEditor(state: AppState, initial: GroupDraft, store: AppStore, isPresented: () -> Boolean) {
    val dismiss = LocalDismissModal.current
    var draft by remember(initial.id) { mutableStateOf(initial) }
    var query by remember { mutableStateOf("") }
    var adding by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }
    val saving = "editor" in state.busy
    val picked = draft.members.mapNotNull { id -> state.bots.firstOrNull { it.id == id } }
    val candidates = roster(state.bots).filter { it.kind != "group" && it.id !in draft.members && it.name.contains(query.trim(), ignoreCase = true) }
    val defaultName = picked.joinToString(", ") { it.name }.ifEmpty { "Name" }
    fun toggle(id: String) {
        draft = draft.copy(members = if (id in draft.members) draft.members - id else draft.members + id)
        query = ""
    }
    Column(Modifier.fillMaxSize().background(Palette.background)) {
        ModalHeader("Group chat") {
            if (saving) Box(Modifier.size(36.dp), contentAlignment = Alignment.Center) { Spinner() }
            else IconButton("Save", "checkmark", enabled = draft.members.isNotEmpty()) {
                store.saveGroup(draft) { dismiss() }
            }
        }
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(start = 20.dp, end = 20.dp, top = 8.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(28.dp)) {
            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
                GroupAvatar(picked.map { AvatarFace(it.avatarShape, it.avatarColor, it.status) }, 84.dp)
                PlainTextField(draft.name, { draft = draft.copy(name = it) }, Modifier.fillMaxWidth().padding(top = 6.dp), defaultName, singleLine = true,
                    style = Type.title2.semibold.copy(textAlign = TextAlign.Center))
                PlainTextField(draft.description, { draft = draft.copy(description = it) }, Modifier.fillMaxWidth(), "What this group works on",
                    style = Type.subheadline.copy(textAlign = TextAlign.Center, color = Palette.secondary), maxLines = 4)
            }
            Column(Modifier.fillMaxWidth().animateContentSize(Motion.layoutSize)) {
                val source = state.bots.firstOrNull { it.id == initial.id }
                if (source != null && state.hello?.get("groupSessionContexts")?.jsonPrimitive?.booleanOrNull == true) {
                    GroupContexts(source, state, store, picked, ::toggle) {
                        // 离开设置去确认私聊摘要前，群资料必须已经保存。
                        isPresented() && draft == initial && !saving
                    }
                } else {
                    Text("Members", Modifier.padding(bottom = 4.dp), style = Type.subheadline.semibold, color = Palette.secondary)
                    for (bot in picked) MemberRow(bot, state.bots) { IconButton("Remove ${bot.name}", "xmark") { toggle(bot.id) } }
                }
                if (adding) {
                    Box(Modifier.padding(vertical = 6.dp)) {
                        FieldBox(query, { query = it }, "Search bots", fill = Palette.surface, imeAction = ImeAction.Done,
                            actions = KeyboardActions(onDone = { candidates.firstOrNull()?.let { toggle(it.id) } }))
                    }
                    for (bot in candidates) Box(Modifier.plainButton { toggle(bot.id) }.semantics(mergeDescendants = true) { contentDescription = "Add ${bot.name}" }) {
                        MemberRow(bot, state.bots, dimmed = true) { Symbol("plus.circle", size = 20.dp, tint = Palette.secondary) }
                    }
                } else if (candidates.isNotEmpty()) Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).plainButton { adding = true },
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                    Box(Modifier.width(32.dp), contentAlignment = Alignment.Center) { Symbol("plus", size = 18.dp, tint = Palette.secondary) }
                    Text("Add Member", style = Type.body, color = Palette.secondary)
                }
                val lead = state.bots.firstOrNull { it.id in draft.members && it.id == (source?.stage?.leaderId ?: source?.groupLeaderId) }
                val route = when {
                    lead != null -> tr("Ask {name} · @ to choose another recipient").replace("{name}", lead.name)
                    !source?.stage?.leaderId.isNullOrEmpty() -> tr("Choose a recipient with @")
                    else -> tr("Everyone answers in turn unless you @mention someone.")
                }
                Text(route, Modifier.padding(top = 10.dp), style = Type.caption, color = Palette.tertiary)
            }
            val roomId = initial.id
            if (roomId != null && state.hello?.get("groupResourceUsage")?.jsonPrimitive?.booleanOrNull == true) {
                GroupResourceUsage(roomId, state, store)
            }
            AnimatedVisibility(state.error != null, enter = fadeIn(Motion.fade()), exit = fadeOut(Motion.fade())) {
                Text(state.error.orEmpty(), style = Type.footnote, color = Palette.danger)
            }
            LinkButton("Delete group chat", color = Palette.danger) { confirmDelete = true }
        }
    }
    val group = state.bots.firstOrNull { it.id == draft.id }
    DeleteBotDialog(group.takeIf { confirmDelete }, state.supportsGroupPurge, { confirmDelete = false }) { target, purge ->
        store.deleteBot(target, purge) { dismiss() }
    }
}

/** 群设置和列表使用同一确认框，由 Host 清理用户明确选中的范围。 */
@Composable internal fun DeleteBotDialog(bot: Bot?, supportsPurge: Boolean, onDismiss: () -> Unit, remove: (Bot, Boolean) -> Unit) {
    val group = bot?.kind == "group"
    val canPurge = group && supportsPurge
    val message = when {
        canPurge -> "Full deletion removes the conversation, exclusive bots and their resources. Copied chats, shared bots and external projects stay."
        group -> "Its bots and their own chats stay."
        else -> "Files it changed on your computer stay as they are."
    }
    CodyncDialog("Delete ${bot?.name ?: "bot"}?", bot != null, onDismiss, message) {
        // 回调捕获目标，弹窗会先清除选中状态再执行动作。
        buildList {
            if (bot != null) {
                add(DialogAction(if (group) "Delete only group chat" else "Delete bot and its conversation", destructive = true) {
                    remove(bot, false)
                })
                if (canPurge) add(DialogAction("Delete group and its bots", destructive = true) { remove(bot, true) })
            }
        }
    }
}

/** A member or candidate: avatar, name, and a trailing control. */
@Composable private fun MemberRow(bot: Bot, bots: List<Bot>, dimmed: Boolean = false, trailing: @Composable () -> Unit) {
    Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
        BotAvatar(bot, bots, 32.dp)
        Text(bot.name, Modifier.weight(1f), style = Type.body, color = if (dimmed) Palette.secondary else Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        trailing()
    }
}

/** 在群资料页读取 Host 汇总，重连后重新读取并保留加载失败提示。 */
@Composable private fun GroupResourceUsage(roomId: String, state: AppState, store: AppStore) {
    var usage by remember(roomId) { mutableStateOf<JsonObject?>(null) }
    var failed by remember(roomId) { mutableStateOf(false) }
    val online = state.actualConnection is LinkState.Ready && state.mismatch == null
    LaunchedEffect(roomId, online) {
        if (online) {
            try {
                usage = store.hostCall("groupResourceUsage", body("roomId" to roomId)) as JsonObject
                failed = false
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                failed = true
            }
        }
    }
    val number = NumberFormat.getIntegerInstance()
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(tr("Resources and usage"), style = Type.subheadline.semibold, color = Palette.text)
        Column(Modifier.fillMaxWidth().background(Palette.surface, RoundedCornerShape(13.dp)).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(9.dp)) {
            val report = usage
            if (report == null) {
                Text(tr(if (failed || !online) "Unavailable" else "Loading…"), style = Type.footnote, color = Palette.secondary)
            } else {
                val bytes = report["resourceBytes"]?.jsonPrimitive?.longOrNull
                GroupUsageReading("Group resource storage", bytes?.let(::formatBytes)
                    ?: tr("Unavailable"), "folder")
                Box(Modifier.fillMaxWidth().height(1.dp).background(Palette.accentDim))
                val tokens = report["groupTokens"]?.jsonPrimitive?.longOrNull
                val share = report["share"]?.jsonPrimitive?.doubleOrNull
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    GroupUsageReading("Group token usage", tokens?.let { "${number.format(it)} Token" }
                        ?: tr("No usage recorded"), "chart.bar")
                    if (share != null) Column(horizontalAlignment = Alignment.End) {
                        val percent = NumberFormat.getPercentInstance().apply { maximumFractionDigits = 1 }
                        Text(percent.format(share), style = Type.callout.semibold, color = Palette.text)
                        Text(tr("Share of host usage"), style = Type.caption, color = Palette.secondary)
                    }
                }
                if (share != null) {
                    Box(Modifier.fillMaxWidth().height(5.dp).background(Palette.accentDim, RoundedCornerShape(3.dp))) {
                        Box(Modifier.fillMaxWidth(share.toFloat()).height(5.dp)
                            .background(Color(0xFF207CFF), RoundedCornerShape(3.dp)))
                    }
                }
                report["hostTokens"]?.jsonPrimitive?.longOrNull?.let {
                    Text(tr("All conversations on this host: {tokens} Token").replace("{tokens}", number.format(it)),
                        style = Type.caption, color = Palette.secondary)
                }
                if (report["partial"]?.jsonPrimitive?.booleanOrNull == true) {
                    Text(tr("Partial usage records"), style = Type.caption, color = Palette.secondary)
                }
                if (failed || !online) {
                    Text(tr("Could not refresh usage."), style = Type.caption, color = Palette.secondary)
                }
            }
        }
    }
}

/** 用相同布局显示资源和 Token 两类读数。 */
@Composable private fun GroupUsageReading(label: String, value: String, icon: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Symbol(icon, size = 18.dp, tint = Palette.secondary)
        Column {
            Text(tr(label), style = Type.footnote, color = Palette.text)
            Text(value, style = Type.callout.semibold, color = Palette.text)
        }
    }
}
