package com.codync.android

import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.Bot
import com.codync.android.core.FastModeState
import com.codync.android.core.GroupFastMode
import com.codync.android.core.InstalledSkill
import com.codync.android.core.fastModeDetail
import com.codync.android.core.fastModeState
import com.codync.android.design.*
import com.kyant.shapes.Capsule
import com.kyant.shapes.RoundedRectangle

/** A command in the composer's `/` list (iOS `SlashCommand`): Fast mode, or one of the bot's skills. */
internal data class SlashCommand(val name: String, val icon: String, val title: String, val detail: String, val warning: Boolean,
    val skill: String? = null, val sessionMode: String? = null)

/**
 * The composer's `/` commands, listed while the draft is just `/partial`, the same in a group (iOS `SlashCommands`): Fast mode
 * runs here (a group's on each member; a new session is in the bot's menu); a skill fills in `/<id> `, which the host turns into
 * "read this skill first".
 */
internal object SlashCommands {
    /** The command name being typed after a leading `/`, before any space. */
    fun query(draft: String): String? = if (draft.startsWith("/") && draft.none(Char::isWhitespace)) draft.drop(1) else null

    /** Built-in commands, then the skills the bot has on (a group's: any member's), whose name or title holds `query`. `members` are a group's bots. */
    /** 列出当前聊天适用的命令与会话切换选项。 */
    fun list(bot: Bot, query: String, skills: List<InstalledSkill>, members: List<Bot> = emptyList()): List<SlashCommand> {
        if (query.startsWith("new")) return sessions(bot).filter { it.name.contains(query) }
        val owners = if (bot.kind == "group") members else listOf(bot)
        val own = skills.filter { skill -> owners.any { skill.id in it.skills } }.map { SlashCommand(it.id, "book.pages", it.name, it.description, false, it.id) }
        return (builtIn(bot, members) + own).filter { query.isEmpty() || it.name.contains(query, true) || it.title.contains(query, true) }
    }

    /** A draft that is just a built-in command (`/fast`): sending it runs the command. */
    /** 解析已确认命令，成员选择与实际发送保持分离。 */
    fun builtIn(bot: Bot, draft: String, members: List<Bot> = emptyList()): SlashCommand? =
        query(draft.trim())?.let { name -> (builtIn(bot, members) + sessions(bot)).firstOrNull { it.name == name } }

    /** /new 默认等任务与保存结束；立即切换和取消切换为另两项。 */
    private fun sessions(bot: Bot): List<SlashCommand> = if (bot.kind == "group") emptyList() else listOf(
        SlashCommand("new", "plus", "在当前任务结束后新建会话", "主会话与群共享；先保存当前执行状态", false, sessionMode = "after"),
        SlashCommand("new-now", "plus", "立即停止并新建", "主会话与群共享；先保存当前执行状态", false, sessionMode = "immediate"),
        SlashCommand("new-cancel", "plus", "取消切换", "主会话与群共享", false, sessionMode = "cancel"),
    )

    /** Fast mode, while the bot (or a member of the group) shows it. */
    private fun builtIn(bot: Bot, members: List<Bot>): List<SlashCommand> {
        val (detail, state) = if (bot.kind == "group") {
            val group = GroupFastMode(members)
            if (group.members.isEmpty()) return emptyList()
            group.detail to group.state
        } else {
            if (!bot.fastModeShown) return emptyList()
            bot.fastModeDetail to bot.fastModeState
        }
        return listOf(SlashCommand("fast", "bolt", "Fast mode", detail, state == FastModeState.Refused))
    }
}

/** The `/` list above the box: each command's icon, name and what it does; tapping one runs it (iOS `CommandList`). */
@Composable internal fun CommandList(commands: List<SlashCommand>, pick: (SlashCommand) -> Unit, modifier: Modifier = Modifier) {
    Column(modifier.fillMaxWidth().heightIn(max = 280.dp).glass(RoundedRectangle(20.dp)).verticalScroll(rememberScrollState())
        .padding(vertical = 6.dp)) {
        for (command in commands) Row(Modifier.fillMaxWidth().plainButton { pick(command) }
            .semantics(mergeDescendants = true) { contentDescription = command.title }.padding(horizontal = 14.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Box(Modifier.width(20.dp), contentAlignment = Alignment.Center) { Symbol(command.icon, size = 17.dp, tint = Palette.secondary) }
            Text(command.title, style = Type.subheadline, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(command.detail, Modifier.weight(1f), style = Type.subheadline, color = if (command.warning) Palette.warning else Palette.secondary,
                maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** 群成员的纵向候选列表，ID 前缀用于区分同名成员。 */
/** The bots to @mention above a group's box: chips with their faces (iOS `SuggestionChip`). */
@Composable internal fun MentionChips(members: List<Bot>, bots: List<Bot>, selected: Int, mention: (Bot) -> Unit) {
    Column(Modifier.fillMaxWidth().heightIn(max = 240.dp).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        for ((index, member) in members.withIndex()) {
        val bring = remember(member.id) { BringIntoViewRequester() }
        LaunchedEffect(selected, member.id) { if (index == selected) bring.bringIntoView() }
        Row(Modifier.bringIntoViewRequester(bring).clip(Capsule()).background(if (index == selected) Palette.surface else androidx.compose.ui.graphics.Color.Transparent).plainButton { mention(member) }
            .padding(horizontal = 10.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            BotAvatar(member, bots, 18.dp)
            Text("${member.name} · ${member.id.take(8)}", style = Type.subheadline, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        }
    }
}

/** The `@partial` being typed at the end of a group's draft, if any. */
/** 只提取光标前的成员查询，保留中间插入之后的正文。 */
internal fun mentionQuery(draft: String, cursor: Int = draft.length): String? {
    val at = draft.lastIndexOf('@', cursor - 1)
    if (at < 0 || at > 0 && !draft[at - 1].isWhitespace()) return null
    val query = draft.substring(at + 1, cursor)
    return if (query.contains('\n') || query.length > 24) null else query
}
