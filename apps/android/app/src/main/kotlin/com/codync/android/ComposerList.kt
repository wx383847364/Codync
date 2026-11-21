package com.codync.android

import androidx.compose.foundation.background
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.Bot
import com.codync.android.design.*
import com.kyant.shapes.RoundedRectangle

/** A list above the box: hugs its rows, scrolls past 280 dp (iOS `ComposerList`). */
@Composable private fun ComposerList(modifier: Modifier, rows: @Composable () -> Unit) {
    Column(modifier.fillMaxWidth().heightIn(max = 280.dp).glass(RoundedRectangle(20.dp)).verticalScroll(rememberScrollState())
        .padding(vertical = 6.dp)) { rows() }
}

/** A row of a list above the box: its icon, name and what it is; tapping it picks it (iOS `ComposerListRow`). */
@Composable private fun ComposerListRow(title: String, detail: String, pick: () -> Unit, warning: Boolean = false, modifier: Modifier = Modifier, icon: @Composable () -> Unit) {
    Row(modifier.fillMaxWidth().plainButton(action = pick)
        .semantics(mergeDescendants = true) { contentDescription = title }.padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Box(Modifier.width(20.dp), contentAlignment = Alignment.Center) { icon() }
        Text(title, style = Type.subheadline, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text(detail, Modifier.weight(1f), style = Type.subheadline, color = if (warning) Palette.warning else Palette.secondary,
            maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** The `/` list: each command's icon, name and what it does; tapping one runs it (iOS `CommandList`). */
/** 复用命令列表的行，方向键移动时把高亮文件滚入可见区。 */
@Composable internal fun CommandList(commands: List<SlashCommand>, pick: (SlashCommand) -> Unit, modifier: Modifier = Modifier,
    selected: Int = 0, status: String = "") {
    ComposerList(modifier) {
        if (status.isNotEmpty()) Text(status, Modifier.padding(horizontal = 14.dp, vertical = 10.dp), color = Palette.secondary, style = Type.subheadline)
        for ((index, command) in commands.withIndex()) {
            val bring = remember(command) { BringIntoViewRequester() }
            LaunchedEffect(selected, command) { if (index == selected) bring.bringIntoView() }
            ComposerListRow(command.title, command.detail, { pick(command) }, command.warning,
                Modifier.bringIntoViewRequester(bring).background(if (index == selected) Palette.surface else androidx.compose.ui.graphics.Color.Transparent)) {
                Symbol(command.icon, size = 17.dp, tint = Palette.secondary)
            }
        }
    }
}

/** 群成员复用统一候选列表，保留稳定身份提示和键盘高亮滚动。 */
@Composable internal fun MentionList(members: List<Bot>, bots: List<Bot>, mention: (Bot) -> Unit, modifier: Modifier = Modifier, selected: Int = 0) {
    ComposerList(modifier) {
        for ((index, member) in members.withIndex()) {
            val bring = remember(member.id) { BringIntoViewRequester() }
            // 键盘循环时只滚动候选，不改变编辑器焦点。
            LaunchedEffect(selected, member.id) { if (index == selected) bring.bringIntoView() }
            ComposerListRow(member.name, "${member.id.take(8)} · ${member.description}", { mention(member) },
                modifier = Modifier.bringIntoViewRequester(bring).background(if (index == selected) Palette.surface else androidx.compose.ui.graphics.Color.Transparent)) {
                BotAvatar(member, bots, 18.dp)
            }
        }
    }
}
