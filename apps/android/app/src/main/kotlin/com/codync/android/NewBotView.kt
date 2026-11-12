package com.codync.android

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.Bot
import com.codync.android.core.BotDraft
import com.codync.android.core.GroupDraft
import com.codync.android.core.LinkState
import com.codync.android.design.*

/** A new bot from its face and name (iOS `NewBotView`); the rest starts on the defaults and changes later in its Settings. */
/** [computers] (ID, label) can take the bot; with more than one it can move to another before it's created ([choose]). */
@Composable fun NewBotView(state: AppState, store: AppStore, computers: List<Pair<String, String>> = emptyList(), choose: (String) -> Unit = {}) {
    var draft by remember { mutableStateOf(BotDraft()) }
    val online = state.actualConnection is LinkState.Ready
    val dismiss = LocalDismissModal.current
    // A new bot starts on an installed agent we know (again on another computer); the host turns on every connector and skill.
    LaunchedEffect(state.computer.id, state.hello) { draft = fillDefaults(draft, state) }
    CreateLayout(state, "New bot", draft.name, { draft = draft.copy(name = it) }, "Name your bot", online, { store.saveBot(draft) { dismiss() } },
        { CharacterAvatar(draft.avatarShape, draft.avatarColor, 96.dp) }) {
        AvatarPicker(draft.avatarShape, draft.avatarColor, { draft = draft.copy(avatarShape = it) }, { draft = draft.copy(avatarColor = it) })
        if (computers.size > 1) ValueRow("Computer") { ChoicePicker(state.computer.id, computers, choose, label = "Computer") }
        if (!online) Text(if (computers.size > 1) "Connect this computer to create the bot, or choose another computer."
            else "Connect this computer to create the bot.", style = Type.caption, color = Palette.warning)
    }
}

/** 建群与新会话共用选择表单，复制会话时预填原有成员。 */
@Composable fun NewGroupView(state: AppState, store: AppStore, source: Bot? = null) {
    var name by remember { mutableStateOf("") }
    var members by remember(source?.id) { mutableStateOf(source?.members.orEmpty()) }
    var leader by remember(source?.id) { mutableStateOf(source?.stage?.leaderId ?: source?.groupLeaderId) }
    val dismiss = LocalDismissModal.current
    val bots = roster(state.bots).filter { it.kind != "group" }
    val picked = members.mapNotNull { id -> state.bots.firstOrNull { it.id == id && !it.deleted && it.kind != "group" } }
    val selectedLead = picked.firstOrNull { it.id == leader }
    val nameDisplay = source?.let { tr("{name} · Auto-numbered").replace("{name}", it.name) }
    val busy = source?.let { "groupSession:${it.id}" } ?: "editor"
    CreateLayout(state, if (source == null) "New group chat" else "Create new group session", name, { name = it },
        picked.joinToString(", ") { it.name }.ifEmpty { "Name your group" }, picked.isNotEmpty(),
        {
            // 新会话由 Host 分配编号，普通群可按成员名称生成标题。
            val ids = picked.map(Bot::id)
            if (source == null) {
                store.saveGroup(GroupDraft(name = name, members = ids,
                    groupLeaderId = selectedLead?.id.takeIf { state.supportsGroupMemberPicker })) { dismiss() }
            } else {
                store.createGroupSession(source.id, ids, selectedLead?.id) { dismiss() }
            }
        }, { GroupAvatar(picked.map { AvatarFace(it.avatarShape, it.avatarColor, it.status) }, 80.dp) },
        group = true, displayName = nameDisplay, busyKey = busy) {
        GroupMemberPicker((bots + source?.memberBots(state.bots).orEmpty()).distinctBy(Bot::id), picked, state.bots) { bot ->
            // 当前负责人离开选择后清空默认接收人。
            if (bot.id in members) {
                members = members - bot.id
                if (leader == bot.id) leader = null
            } else members = members + bot.id
        }
        if (state.supportsGroupMemberPicker) GroupBotPicker(picked, state.bots, selectedLead?.id, "Group lead (optional)", allowsNone = true) { leader = it }
        Text(when {
            source != null -> tr("Original members are preselected. You can change them.")
            selectedLead != null -> tr("Ask {name} · @ to choose another recipient").replace("{name}", selectedLead.name)
            else -> tr("Ask everyone · @ to ask one bot")
        }, style = Type.caption, color = Palette.secondary)
    }
}

/** 输入筛选或展开完整名单；下方四列网格只保留已加入成员。 */
@Composable internal fun GroupMemberPicker(bots: List<Bot>, picked: List<Bot>, roster: List<Bot>, toggle: (Bot) -> Unit) {
    var query by remember { mutableStateOf("") }
    var open by remember { mutableStateOf(false) }
    val results = bots.filter { it.name.contains(query.trim(), ignoreCase = true) }
    val selected = picked.mapTo(mutableSetOf(), Bot::id)
    val shape = RoundedCornerShape(12.dp)
    Row(Modifier.fillMaxWidth().background(Palette.surface, shape).padding(start = 12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Symbol("magnifyingglass", size = 16.dp, tint = Palette.secondary)
        PlainTextField(query, {
            // 编辑搜索词时显示候选，无需另点展开。
            query = it
            open = true
        }, Modifier.weight(1f), "Search or add bots", singleLine = true)
        IconButton("Show bot list", if (open) "chevron.up" else "chevron.down") {
            // 通过箭头重开时查看完整名单。
            if (!open) query = ""
            open = !open
        }
    }
    AnimatedVisibility(open, enter = fadeIn(Motion.fade()), exit = fadeOut(Motion.fade())) {
        GroupBotOptions(results, roster, selected, multiple = true) { bot ->
            // 多选时保留展开状态，以便连续加入多个成员。
            bot?.let(toggle)
        }
    }
    for (row in picked.chunked(4)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for (bot in row) {
                // 已加入的头像可直接移除。
                MemberTile(bot, roster, Modifier.weight(1f)) { toggle(bot) }
            }
            repeat(4 - row.size) { Spacer(Modifier.weight(1f)) }
        }
    }
}

/** 三种列表使用相同高度与头像行，只有多选时给未加入成员显示加号。 */
@Composable private fun GroupBotOptions(bots: List<Bot>, roster: List<Bot>, selected: Set<String>, multiple: Boolean = false,
    allowsNone: Boolean = false, pick: (Bot?) -> Unit) {
    if (bots.isEmpty() && !allowsNone) {
        Text(tr("No matching bots"), Modifier.padding(8.dp), style = Type.caption, color = Palette.secondary)
    } else {
        LazyColumn(Modifier.fillMaxWidth().heightIn(max = 220.dp).background(Palette.surface, RoundedCornerShape(12.dp))) {
            if (allowsNone) item {
                GroupBotOption(null, roster, selected.isEmpty(), multiple) { pick(null) }
            }
            items(bots, key = Bot::id) { bot ->
                GroupBotOption(bot, roster, bot.id in selected, multiple) { pick(bot) }
            }
        }
    }
}

/** 一行显示头像、名称和状态；无负责人选项预留头像位置。 */
@Composable private fun GroupBotOption(bot: Bot?, roster: List<Bot>, on: Boolean, multiple: Boolean, pick: () -> Unit) {
    val name = bot?.name ?: tr("No lead")
    Row(Modifier.fillMaxWidth().choice(on, name, if (multiple) Role.Checkbox else Role.RadioButton, pick).heightIn(min = 44.dp).padding(10.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        if (bot == null) Spacer(Modifier.size(24.dp)) else BotAvatar(bot, roster, 24.dp)
        Text(name, Modifier.weight(1f), style = Type.body, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (on || multiple) Symbol(if (on) "checkmark" else "plus", size = 14.dp, tint = Palette.text)
    }
}

/** 负责人和角色选择在触发控件下方同宽展开，单选完成后收起。 */
@Composable internal fun GroupBotPicker(bots: List<Bot>, roster: List<Bot>, selection: String?, label: String,
    allowsNone: Boolean = false, showsLabel: Boolean = true, onChange: (String?) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val current = bots.firstOrNull { it.id == selection }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(Modifier.fillMaxWidth().background(Palette.surface, RoundedCornerShape(12.dp))
            .clickable(enabled = bots.isNotEmpty(), role = Role.Button) { open = !open }.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (showsLabel) Text(tr(label), Modifier.weight(1f), style = Type.body, color = Palette.text,
                maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(current?.name ?: tr(if (allowsNone) "No lead" else "Choose"),
                modifier = Modifier.weight(1f), style = Type.body,
                textAlign = if (showsLabel) TextAlign.End else TextAlign.Start,
                color = if (showsLabel) Palette.secondary else Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Symbol(if (open) "chevron.up" else "chevron.down", size = 12.dp, tint = Palette.secondary)
        }
        AnimatedVisibility(open, enter = fadeIn(Motion.fade()), exit = fadeOut(Motion.fade())) {
            GroupBotOptions(bots, roster, setOfNotNull(current?.id), allowsNone = allowsNone) { bot ->
                onChange(bot?.id)
                open = false
            }
        }
    }
}

/** Both create dialogs' layout (iOS `CreateForm`): a big preview, the centered name (Done creates), the grids, then any save error. */
@Composable internal fun CreateLayout(state: AppState, title: String, name: String, rename: (String) -> Unit, prompt: String, canCreate: Boolean,
    create: () -> Unit, preview: @Composable () -> Unit, group: Boolean = false, displayName: String? = null,
    busyKey: String = "editor", content: @Composable ColumnScope.() -> Unit) {
    val saving = busyKey in state.busy
    val focus = remember { FocusRequester() }
    LaunchedEffect(displayName) { if (!group && displayName == null) focus.requestFocus() }
    Column(Modifier.fillMaxSize().background(Palette.background)) {
        ModalHeader(title) {
            if (saving) Box(Modifier.size(36.dp), contentAlignment = Alignment.Center) { Spinner() }
            else IconButton("Create", "checkmark", enabled = canCreate, action = create)
        }
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(start = 20.dp, end = 20.dp, top = 8.dp, bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(if (group) 12.dp else 24.dp)) {
            preview()
            if (displayName != null) Text(displayName, Modifier.fillMaxWidth()
                .background(Palette.surface, RoundedCornerShape(12.dp)).padding(12.dp), style = Type.body, color = Palette.text)
            else if (group) FieldBox(name, rename, prompt, fill = Palette.surface, imeAction = ImeAction.Done,
                actions = KeyboardActions(onDone = { if (canCreate && !saving) create() }))
            else PlainTextField(name, rename, Modifier.fillMaxWidth().focusRequester(focus), prompt, singleLine = true,
                style = Type.title2.semibold.copy(textAlign = TextAlign.Center),
                keyboard = KeyboardOptions(capitalization = KeyboardCapitalization.Words, imeAction = ImeAction.Done),
                actions = KeyboardActions(onDone = { if (canCreate && !saving) create() }))
            content()
            AnimatedVisibility(state.error != null, enter = fadeIn(Motion.fade()), exit = fadeOut(Motion.fade())) {
                Text(state.error.orEmpty(), style = Type.footnote, color = Palette.danger)
            }
        }
    }
}

/** 用头像圆环显示已加入状态，点击即可从当前表单移除。 */
@Composable private fun MemberTile(bot: Bot, bots: List<Bot>, modifier: Modifier, toggle: () -> Unit) {
    Column(modifier.choice(true, bot.name, Role.Checkbox, toggle), horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Box(Modifier.ring(true)) { BotAvatar(bot, bots, 44.dp) }
        Text(bot.name, style = Type.caption, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** Picks an installed agent we know (a registry agent nobody chose is never the default). */
private fun fillDefaults(draft: BotDraft, state: AppState): BotDraft {
    val backends = backends(state)
    if (backends.any { it.text("id") == draft.backend && it.available }) return draft
    return backends.firstOrNull { it.available && it.curated }?.text("id")?.let { draft.copy(backend = it) } ?: draft
}
