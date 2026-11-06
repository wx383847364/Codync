package com.codync.android

import android.content.ClipDescription
import android.content.ClipboardManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.content.ReceiveContentListener
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isMetaPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.style.TextOverflow
import com.codync.android.core.*
import com.codync.android.design.*
import com.kyant.shapes.Capsule
import com.kyant.shapes.RoundedRectangle
import kotlinx.coroutines.launch

/** What the composer's one trailing button does now; it morphs between them. */
private enum class Trailing { Interrupt, Stop, Call, Send }

/**
 * The message box under a chat, as iOS `Composer`: a glass + for files, a glass field whose
 * trailing button morphs between call, send, stop and interrupt. `onInterrupt` cuts the bot off
 * while it speaks in a voice call. `replyTo` is the message the next send quotes, shown on top. `about`, in an artifact's
 * viewer: the artifact's title, which the box names in place of the quote (its card) it sends with; `draftKey` is then the
 * artifact's draft, apart from the chat's. `queued`: the user's messages waiting behind the bot's work, shown on top ([QueuedMessages]).
 */
@OptIn(ExperimentalFoundationApi::class)
/** 维护原生输入法、光标候选与结构化身份，选人不发送。 */
@Composable fun MessageComposer(bot: Bot, state: AppState, store: AppStore, onCall: (() -> Unit)?,
    onInterrupt: (() -> Unit)? = null, replyTo: ReplyQuote? = null, cancelReply: () -> Unit = {}, about: String? = null,
    draftKey: String = bot.id, queued: List<Entry> = emptyList()) {
    val draft = state.drafts[draftKey]
    val text = rememberTextFieldState(initialText = draft?.text.orEmpty())
    val files = draft?.files.orEmpty()
    val queuing = "draft-send:$draftKey" in state.busy
    val importing = "attachments" in state.busy
    // What this box sent to the store, so a draft set elsewhere (a routine request) can be told from our own echo.
    // 只记录原生编辑范围，转换回调不修改Compose或Store状态。
    val edits = remember(draftKey) { ArrayDeque<Triple<String, String, IntRange>>() }
    val transformation = remember(draftKey) { InputTransformation {
        if (changes.changeCount > 0) {
            val ranges = (0 until changes.changeCount).map { changes.getOriginalRange(it) }
            val from = ranges.minOf { it.min }
            val to = ranges.maxOf { it.max }
            edits.addLast(Triple(originalText.toString(), asCharSequence().toString(), from until to))
        }
    } }
    val echoes = remember(draftKey) { ArrayDeque<String>() }
    LaunchedEffect(text, draftKey) {
        snapshotFlow { text.text.toString() }.collect {
            echoes.addLast(it); while (echoes.size > 32) echoes.removeFirst()
            while (edits.isNotEmpty()) {
                val (before, after, range) = edits.removeFirst()
                if (store.state.value.drafts[draftKey]?.text.orEmpty() == before) store.editDraft(draftKey, after, range)
            }
            store.editDraft(draftKey, it)
        }
    }
    // Choosing a message to reply to goes straight to typing.
    val focus = remember { FocusRequester() }
    LaunchedEffect(replyTo?.id) { if (replyTo != null && about == null) focus.requestFocus() }
    LaunchedEffect(draft?.text) {
        val external = draft?.text.orEmpty()
        if (external != text.text.toString() && external !in echoes) text.setTextAndPlaceCursorAtEnd(external)
    }
    val clipboard = LocalClipboard.current
    val context = LocalContext.current
    val clips = context.getSystemService(ClipboardManager::class.java)
    val scope = rememberCoroutineScope()
    // Dictation stops with its chat or draft (another chat, an artifact's box) and when the box goes.
    val dictation = remember(draftKey) { Dictation(context, scope) }
    DisposableEffect(dictation) { onDispose { dictation.stop() } }
    val photos = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(10)) { uris ->
        if (uris.isNotEmpty()) store.importFiles(uris) { store.addDraftFiles(draftKey, it) }
    }
    val documents = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isNotEmpty()) store.importFiles(uris) { store.addDraftFiles(draftKey, it) }
    }
    val receiver = remember(draftKey, importing, queuing) { ReceiveContentListener { content ->
        if (importing || queuing) content else {
            val uris = mutableListOf<android.net.Uri>()
            val remaining = content.consume { item -> item.uri?.takeIf { it.scheme == "content" }?.let { uris.add(it); true } ?: false }
            if (uris.isNotEmpty()) store.importFiles(uris) { store.addDraftFiles(draftKey, it) }
            remaining
        }
    } }
    val online = state.actualConnection is LinkState.Ready
    val empty = text.text.isBlank() && files.isEmpty()
    // Offline computers still take text when the relay can hold it; files need the computer.
    val canSend = !empty && !importing && !queuing && (online || state.actualConnection is LinkState.ComputerOffline && files.isEmpty())
    val working = bot.isWorking(bot.id)
    val bare = text.text.isEmpty() && files.isEmpty()
    val trailing = when {
        onInterrupt != null && bare -> Trailing.Interrupt
        working && bare -> Trailing.Stop
        onCall != null && bare -> Trailing.Call
        else -> Trailing.Send
    }
    val placeholder = when {
        about != null -> tr("Message about “{title}”").replace("{title}", about)
        replyTo != null -> tr("Reply…")
        bot.kind == "group" -> tr("Ask everyone · @ to ask one bot")
        working -> tr("Queue a message for {name}").replace("{name}", bot.name)
        else -> tr("Ask {name}").replace("{name}", bot.name)
    }
    val typed = text.text.toString()
    val members = bot.memberBots(state.bots)
    val commands = SlashCommands.query(typed)?.let { SlashCommands.list(bot, it, state.plugins?.skills.orEmpty(), members) }.orEmpty()
    // Starting a command: list the computer's skills again, once each time the draft becomes "/".
    val startingCommand = typed == "/"
    LaunchedEffect(startingCommand) { if (startingCommand && online) store.refreshPlugins() }
    // The one way a group's box differs: @ asks one of its bots.
    val mentionKey = "${text.selection.start}:${text.selection.end}:$typed"
    var mentionIndex by remember(mentionKey) { mutableStateOf(0) }
    // 关闭仅作用于当前查询，编辑或移动光标后解除临时关闭。
    var dismissedMention by remember(mentionKey) { mutableStateOf<String?>(null) }
    val mentions = if (dismissedMention == mentionKey || bot.kind != "group") emptyList() else mentionQuery(typed, text.selection.start)?.takeIf { text.selection.collapsed }?.let { query ->
        members.filter { query.isEmpty() || it.name.contains(query, true) }
    }.orEmpty()
    /** 原子替换查询并恢复光标，尾文与后方绑定一起保留。 */
    fun selectMention(member: Bot) {
        val end = text.selection.start
        val start = typed.lastIndexOf('@', end - 1)
        if (start < 0) return
        val token = "@${member.name} "
        store.bindMention(draftKey, typed.substring(0, start) + token + typed.substring(end), Mention(member.id, start, start + token.length - 1))
        text.edit { replace(start, end, token); selection = androidx.compose.ui.text.TextRange(start + token.length) }
        dismissedMention = mentionKey
    }
    fun clear() = text.edit { replace(0, length, "") }
    /** 把群快捷操作按成员发送给 host，统一执行会话切换语义。 */
    fun run(command: SlashCommand) {
        val skill = command.skill
        when {
            command.sessionMode != null -> { store.command("newSession", body("botId" to bot.id, "mode" to command.sessionMode)); clear() }
            skill != null -> text.setTextAndPlaceCursorAtEnd("/$skill ")
            else -> { store.toggleFastMode(bot); clear() }
        }
    }
    fun send() {
        // Words still on their way would land in the emptied box.
        dictation.stop()
        val command = if (files.isEmpty()) SlashCommands.builtIn(bot, typed, members) else null
        if (command != null) run(command) else store.sendDraft(draftKey, bot.id, typed, replyTo) { clear(); cancelReply() }
    }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
    // 来源群直接展示成员保存故障，按持有内存结果的成员身份重试。
    (if (bot.kind == "group") members.filter { it.workingChat == bot.id } else listOf(bot)).filter { it.storageError != null }.forEach { member ->
        Text("${member.name}: ${member.storageError}", style = Type.footnote, color = Palette.danger)
        SecondaryButton("重试保存", action = { store.command("retryDeliverySave", body("botId" to member.id)) })
    }
    if (bot.pendingNewSession) Text("将在当前任务结束后新建会话；主会话与群共享。输入 /new-cancel 取消切换。", style = Type.footnote, color = Palette.secondary)
    // Over the box, past the + button.
    QueuedMessages(queued, state, store, Modifier.padding(start = 16.dp + FIELD_HEIGHT + 8.dp, end = 16.dp))
    AnimatedVisibility(commands.isNotEmpty(), enter = fadeIn(Motion.fade()) + expandVertically(Motion.layoutSize),
        exit = fadeOut(Motion.fade()) + shrinkVertically(Motion.layoutSize)) {
        CommandList(commands, ::run, Modifier.padding(horizontal = 16.dp))
    }
    AnimatedVisibility(mentions.isNotEmpty(), enter = fadeIn(Motion.fade()), exit = fadeOut(Motion.fade())) {
        MentionChips(mentions, state.bots, mentionIndex, ::selectMention)
    }
    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 6.dp, bottom = 16.dp), verticalAlignment = Alignment.Bottom,
        horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        DropdownMenu({ buildList {
            add(MenuItem("Photos", icon = "photo.on.rectangle") { photos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) })
            add(MenuItem("Files", icon = "folder") { documents.launch(arrayOf("*/*")) })
            // Only the clip's description: reading the clip itself would show Android's paste notice.
            if (clipOffersFiles(clips.primaryClipDescription?.mimeTypes().orEmpty())) add(MenuItem("Paste", icon = "doc.on.clipboard") { scope.launch {
                val data = clipboard.getClipEntry()?.clipData
                val uris = if (data == null) emptyList() else (0 until data.itemCount).mapNotNull { data.getItemAt(it).uri?.takeIf { uri -> uri.scheme == "content" } }
                if (uris.isNotEmpty()) store.importFiles(uris) { store.addDraftFiles(draftKey, it) }
            } })
        } }, Modifier.size(FIELD_HEIGHT).glass(Capsule()), enabled = !queuing, label = "Add files") {
            Symbol("plus", size = 26.dp, tint = Palette.text)
        }
        Column(Modifier.weight(1f).glass(RoundedRectangle(24.dp)).animateContentSize(Motion.layoutSize)) {
            // Each quote keeps its own bar through the transition, so a cancelled one fades out with its text.
            AnimatedContent(replyTo?.text?.takeIf { about == null }, transitionSpec = { fadeIn(Motion.fade()) togetherWith fadeOut(Motion.fade()) },
                label = "Reply") { quote ->
                if (quote != null) Row(Modifier.padding(start = 18.dp, end = 6.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    QuoteLine(quote, Modifier.weight(1f))
                    IconButton("Cancel reply", "xmark", action = cancelReply)
                }
            }
            if (files.isNotEmpty()) Row(Modifier.horizontalScroll(rememberScrollState()).padding(start = 10.dp, end = 10.dp, top = 8.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (file in files) ComposerAttachment(file) { store.removeDraftFile(draftKey, file) }
            }
            Row(Modifier.padding(start = 18.dp, end = 6.dp, top = 6.dp, bottom = 6.dp), verticalAlignment = Alignment.Bottom,
                horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                // A hardware keyboard's Enter is a new line; Ctrl/Meta+Enter (⌘↩ on iOS) sends, and never stops or calls.
                val sendKey = Modifier.onPreviewKeyEvent { e ->
                    // 原生composition未提交时，Enter/方向键全部交给输入法。
                    if (e.type == KeyEventType.KeyDown && text.composition == null && mentions.isNotEmpty() && !e.isCtrlPressed && !e.isMetaPressed) {
                        val step = when(e.key) { Key.DirectionDown -> 1; Key.DirectionUp -> -1; else -> 0 }
                        when {
                            step != 0 -> { mentionIndex = (mentionIndex + step + mentions.size) % mentions.size; return@onPreviewKeyEvent true }
                            e.key == Key.Escape -> { dismissedMention = mentionKey; return@onPreviewKeyEvent true }
                            !e.isShiftPressed && (e.key == Key.Enter || e.key == Key.Tab) -> {
                                if (e.nativeKeyEvent.repeatCount == 0) selectMention(mentions[mentionIndex.coerceIn(0, mentions.lastIndex)])
                                return@onPreviewKeyEvent true
                            }
                        }
                    }
                    val ctrlEnter = e.type == KeyEventType.KeyDown && e.key == Key.Enter && (e.isCtrlPressed || e.isMetaPressed)
                    if (ctrlEnter && trailing == Trailing.Send && canSend) send()
                    ctrlEnter
                }
                BasicTextField(state = text, inputTransformation = transformation, enabled = !queuing, modifier = Modifier.weight(1f).focusRequester(focus).contentReceiver(receiver).then(sendKey).padding(vertical = 10.dp),
                    textStyle = Type.body.copy(color = Palette.text), cursorBrush = SolidColor(Palette.accent),
                    lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 8),
                    decorator = { input -> Box { if (text.text.isEmpty()) Text(placeholder, style = Type.body, color = Palette.tertiary, maxLines = 1, overflow = TextOverflow.Ellipsis); input() } })
                Box(Modifier.padding(bottom = 4.dp)) {
                    DictationButton(dictation, state, store, 34.dp, { text.text.toString() }) { change ->
                        text.setTextAndPlaceCursorAtEnd(change(text.text.toString()))
                    }
                }
                TrailingButton(trailing, canSend, Modifier.padding(bottom = 4.dp)) {
                    when (trailing) {
                        Trailing.Interrupt -> onInterrupt?.invoke()
                        Trailing.Stop -> store.command("stop", body("botId" to bot.id))
                        // The call takes the microphone.
                        Trailing.Call -> { dictation.stop(); onCall?.invoke() }
                        Trailing.Send -> send()
                    }
                }
            }
        }
    }
    }
}

private val FIELD_HEIGHT = 54.dp

/** One button that morphs between call, send, stop and interrupt: its symbol swaps, its fill crossfades and the capsule narrows. */
@Composable private fun TrailingButton(state: Trailing, canSend: Boolean, modifier: Modifier, action: () -> Unit) {
    val enabled = state != Trailing.Send || canSend
    val fill by animateColorAsState(when {
        state == Trailing.Interrupt -> Palette.danger
        state == Trailing.Send && !canSend -> Palette.accentDim
        else -> Palette.accentFill
    }, Motion.morph(), label = "Fill")
    val ink by animateColorAsState(when {
        state == Trailing.Interrupt -> Color.White
        state == Trailing.Send && !canSend -> Palette.tertiary
        else -> Palette.onAccent
    }, Motion.morph(), label = "Ink")
    val width by animateDpAsState(if (state == Trailing.Call) 46.dp else 34.dp, Motion.morph(), label = "Width")
    val (source, pressed) = rememberPress()
    Box(modifier.pressScale(pressed).size(width, 34.dp).clip(Capsule()).background(fill)
        .clickable(source, null, enabled = enabled, role = Role.Button, onClick = action)
        .semantics { contentDescription = when (state) { Trailing.Interrupt -> "Interrupt"; Trailing.Stop -> "Stop"; Trailing.Call -> "Call"; Trailing.Send -> "Send" } },
        contentAlignment = Alignment.Center) {
        AnimatedContent(state, transitionSpec = { (fadeIn(Motion.morph()) + scaleIn(Motion.morph(), 0.6f)) togetherWith
            (fadeOut(Motion.morph()) + scaleOut(Motion.morph(), 0.6f)) }, label = "Symbol") { shown ->
            when (shown) {
                Trailing.Interrupt, Trailing.Stop -> Symbol("stop.fill", size = 16.dp, tint = ink)
                Trailing.Call -> Symbol("waveform", size = 20.dp, tint = ink)
                Trailing.Send -> Symbol("arrow.up", size = 19.dp, tint = ink)
            }
        }
    }
}

/**
 * Whether the clipboard holds what Paste can attach (iOS: images or URLs), from its MIME types alone.
 * Text, HTML and intents carry no file; any other type comes from a URI item.
 */
internal fun clipOffersFiles(mimeTypes: List<String>): Boolean =
    mimeTypes.any { it !in listOf(ClipDescription.MIMETYPE_TEXT_PLAIN, ClipDescription.MIMETYPE_TEXT_HTML, ClipDescription.MIMETYPE_TEXT_INTENT) }

private fun ClipDescription.mimeTypes(): List<String> = (0 until mimeTypeCount).map(::getMimeType)

fun formatBytes(size: Long): String = when {
    size >= 1024 * 1024 -> "%.1f MB".format(size / (1024.0 * 1024))
    size >= 1024 -> "%.0f KB".format(size / 1024.0)
    else -> "$size bytes"
}
