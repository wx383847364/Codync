package com.codync.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.codync.android.core.Bot
import com.codync.android.core.BotDraft
import com.codync.android.core.BotSessionUsage
import com.codync.android.core.LinkState
import com.codync.android.core.ScreenState
import com.codync.android.core.hasLibrary
import com.codync.android.design.*
import java.io.IOException
import java.text.DateFormat
import java.text.NumberFormat
import java.util.Date
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.coroutines.delay

/** The details sheet's pages, as on the desktop inspector (iOS `BotDetailsView.Tab`). */
internal enum class DetailsTab(val title: String) { Details("Details"), Library("Library"), Memory("Memory"), Settings("Settings") }

/**
 * What to open the details sheet on (iOS `DetailsRequest`): a page, a routine's editor on the Details page, or another bot's
 * details than the chat's (a group member's, from its notice).
 */
internal data class DetailsRequest(val tab: DetailsTab = DetailsTab.Details, val routineId: String? = null, val botId: String? = null)

/**
 * A bot's details (iOS `BotDetailsView`): who it is, then its details, library and settings. Settings save as you edit.
 * `openArtifact`: an artifact picked in the Library, for the chat's viewer.
 */
@Composable internal fun BotDetailsView(bot: Bot, request: DetailsRequest, state: AppState, store: AppStore, openArtifact: (OpenArtifact) -> Unit) {
    AnalyticsOnAppear(AnalyticsEvent.ScreenViewed(AnalyticsScreen.BotDetails))
    val dismiss = LocalDismissModal.current
    var tab by remember(request) { mutableStateOf(request.tab) }
    // A notice's routine opens its editor once, not again each time the Details page comes back.
    var routineId by remember(request) { mutableStateOf(request.routineId) }
    val saved = BotDraft.from(bot)
    var draft by rememberBotDraft(saved, state)
    var template by remember { mutableStateOf<BotDraft?>(null) }
    val online = state.actualConnection is LinkState.Ready
    // What the last save said, on the page where it was made (iOS `saveError`); one that fails after the page closed
    // shows over the app instead.
    var saveError by remember(bot.id) { mutableStateOf<String?>(null) }
    val shown = remember { mutableStateOf(true) }
    DisposableEffect(Unit) { onDispose { shown.value = false } }
    // Saves a moment after the last change, like the desktop inspector. Keyed on the saved bot too: an edit made while
    // the previous save was still out (and so skipped) is sent once that save lands.
    LaunchedEffect(draft, online, saved) {
        if (!online || !draft.isValid || draft == saved) return@LaunchedEffect
        delay(600)
        store.saveSettings(draft) { error -> if (shown.value) saveError = error else error?.let { store.showError(IOException(it)) } }
    }
    val tabs = listOfNotNull(DetailsTab.Details, DetailsTab.Library.takeIf { hasLibrary(state.hello?.text("version")) }, DetailsTab.Memory, DetailsTab.Settings)
    Box(Modifier.fillMaxSize().background(Palette.background)) {
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(start = 20.dp, end = 20.dp, bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally) {
            Identity(bot, state)
            TabChoice(tab, tabs.map { it to it.title }, { tab = it }, Modifier.padding(top = 18.dp))
            CompositionLocalProvider(LocalCardStyle provides CardStyle.Outlined) {
                Box(Modifier.fillMaxWidth().padding(top = 20.dp)) {
                    when (tab) {
                        DetailsTab.Details -> Column(verticalArrangement = Arrangement.spacedBy(28.dp)) {
                            if (state.hello?.get("botWorkspaceUsage")?.jsonPrimitive?.booleanOrNull == true) {
                                BotWorkspaceUsage(bot.id, store, online)
                            }
                            if (bot.devServers.isNotEmpty()) DevServersSection(bot, store)
                            BotSettingsForm(state, store, draft, { draft = it }, FormPart.Often, error = saveError)
                            state.screen?.let { ComputerCard(it, draft.computer, online) { on -> draft = draft.copy(computer = on) } }
                            if (state.hello?.get("botSessionUsage")?.jsonPrimitive?.booleanOrNull == true) {
                                SessionUsage(bot, store, online, request.botId == null, dismiss,
                                    state.hello.get("sessionHistory")?.jsonPrimitive?.booleanOrNull == true, state.entries)
                            }
                            RoutinesView(bot, state, store, routineId)
                            LaunchedEffect(Unit) { routineId = null }
                        }
                        DetailsTab.Library -> LibraryView(bot, state, store, openArtifact)
                        DetailsTab.Memory -> MemoryCard(bot.id, state, store)
                        DetailsTab.Settings -> BotSettingsForm(state, store, draft, { draft = it }, FormPart.Rarely, error = saveError)
                    }
                }
            }
        }
        Row(Modifier.align(Alignment.TopEnd).padding(top = 14.dp, end = 14.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            // The bot as it's saved, as on the iPhone.
            CornerButton("Create template", "square.and.arrow.up") { template = saved }
            CornerButton("Close details", "xmark", dismiss)
        }
    }
    CodyncSheet(template, { template = null }) { BotTemplateView(it) }
}

/** The bot's avatar and name, and what runs it. */
@Composable private fun Identity(bot: Bot, state: AppState) {
    Column(Modifier.fillMaxWidth().padding(top = 64.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Box(Modifier.padding(bottom = 10.dp)) {
            BotAvatar(bot, state.bots, 72.dp)
            if (bot.lead) Box(Modifier.align(Alignment.BottomEnd).size(28.dp), contentAlignment = Alignment.Center) {
                LeadAgentBadge(28.dp, interactive = true)
            }
        }
        Text(bot.name, style = Type.title3.semibold, color = Palette.text, textAlign = TextAlign.Center)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            AgentIcon(agentRegistry(state, bot.backend), 13.dp)
            Text(backendName(state, bot.backend) + bot.model?.let { " · $it" }.orEmpty(), style = Type.footnote, color = Palette.secondary)
        }
    }
}

/** "Use the computer", saved with the rest of the draft, so a save of the other settings never sends it back. */
@Composable private fun ComputerCard(screen: ScreenState, on: Boolean, online: Boolean, change: (Boolean) -> Unit) = CardSection("Computer") {
    row {
        SwitchRow("Use the computer", if (screen.controlReady) null else "On that computer, open Codync → Settings → Computer access to set it up first.",
            on, change, enabled = online)
    }
}

/** Grok Bot's corner buttons: the page color, ringed by a soft shadow. */
@Composable private fun CornerButton(title: String, symbol: String, action: () -> Unit) {
    val label = tr(title)
    Box(Modifier.size(36.dp).shadow(1.dp, CircleShape).background(Palette.background, CircleShape).plainButton(action = action)
        .semantics { contentDescription = label }, contentAlignment = Alignment.Center) {
        Symbol(symbol, size = 17.dp, tint = Palette.text)
    }
}


/** 显示远程 Host 上的 Bot 托管工作区大小。 */
@Composable private fun BotWorkspaceUsage(botId: String, store: AppStore, online: Boolean) {
    var bytes by remember(botId) { mutableStateOf<Long?>(null) }
    var loaded by remember(botId) { mutableStateOf(false) }
    val context = LocalContext.current
    // 重连时重新扫描，取消页面读取时保留协程取消的语义。
    LaunchedEffect(botId, online) {
        if (!online) return@LaunchedEffect
        loaded = false
        try {
            val result = store.hostCall("botWorkspaceUsage", body("botId" to botId)) as JsonObject
            bytes = result["bytes"]?.jsonPrimitive?.longOrNull
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            bytes = null
        }
        loaded = true
    }
    val label = when {
        !online -> tr("Unavailable")
        !loaded -> tr("Loading…")
        bytes == 0L -> "0 B"
        else -> bytes?.let { android.text.format.Formatter.formatShortFileSize(context, it) } ?: tr("Unavailable")
    }
    CardSection("Disk usage") {
        row {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(tr("Managed workspace"))
                Text(label, color = Palette.secondary)
            }
        }
    }
}

/** 展示主会话实报占用，展开尝试查看独立的输入、输出和缓存字段。 */
@Composable private fun SessionUsage(bot: Bot, store: AppStore, online: Boolean, offersSessionHandoff: Boolean, dismiss: () -> Unit, supportsHistory: Boolean, entries: List<com.codync.android.core.Entry>) {
    val scope = rememberCoroutineScope()
    var usage by remember(bot.id) { mutableStateOf<BotSessionUsage?>(null) }
    var error by remember(bot.id) { mutableStateOf<String?>(null) }
    var refresh by remember(bot.id) { mutableStateOf(0) }
    var preparing by remember(bot.id) { mutableStateOf(false) }
    var expanded by remember(bot.id) { mutableStateOf<String?>(null) }
    val formatter = NumberFormat.getIntegerInstance()
    val translate = translator()

    /** 已知零与缺报的值分别显示。 */
    fun number(value: Long?) = value?.let(formatter::format) ?: translate("Unavailable")

    // 详情打开、任务状态改变或手动刷新后读取最新快照。
    LaunchedEffect(bot.id, bot.status, bot.mainSessionRevision, online, refresh) {
        if (!online) return@LaunchedEffect
        try {
            usage = store.botSessionUsage(bot.id)
            error = null
        } catch (failure: CancellationException) {
            throw failure
        } catch (failure: Exception) {
            error = failure.localizedMessage
        }
    }
    val current = usage
    val context = when {
        current == null -> tr(if (online) "Loading…" else "Unavailable")
        !current.hasSession -> tr("No active main session.")
        else -> "${number(current.used)} / ${number(current.size)}"
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        CardSection("Main session context", accessory = {
            IconButton("Refresh usage", "arrow.clockwise", enabled = online) { refresh += 1 }
        }) {
            row { Text(context) }
        }
        if (offersSessionHandoff && supportsHistory) SessionHistory(bot.id, store, online, entries = entries)
        error?.let { Text(tr(it), style = Type.footnote, color = Palette.danger) }
        if (offersSessionHandoff && current?.hasSession == true) {
            SecondaryButton("Prepare new session with summary", enabled = online && !bot.isWorking && !preparing) {
                // 成功后返回聊天，方便检查即将带入新会话的摘要。
                scope.launch {
                    preparing = true
                    error = null
                    try {
                        store.prepareHandoff(bot.id)
                        dismiss()
                    } catch (failure: CancellationException) {
                        throw failure
                    } catch (failure: Exception) {
                        error = failure.localizedMessage
                    } finally {
                        preparing = false
                    }
                }
            }
        }
        if (current != null) {
            CardSection("Recent task attempts") {
                if (current.attempts.isEmpty()) row { Text(tr("No recent task attempts")) }
                current.attempts.forEach { attempt ->
                    row {
                        val date = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(attempt.at))
                        val subject = "$date · ${attempt.backend} · ${attempt.model ?: tr("Unknown model")}"
                        Disclosure(expanded == attempt.id, { expanded = if (expanded == attempt.id) null else attempt.id }, Modifier.fillMaxWidth(), {
                            Text(subject, style = Type.footnote)
                        }) {
                            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                for (metric in attempt.rows) {
                                    Text("${tr(metric.label)} · ${number(metric.value)}", style = Type.footnote)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
