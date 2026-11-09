package com.codync.android

import android.content.ClipData
import android.content.ClipboardManager
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import coil3.ImageLoader
import com.codync.android.core.MarkdownBlocks
import com.codync.android.core.MarkdownBlocks.Block
import com.codync.android.design.*
import com.kyant.shapes.RoundedRectangle
import org.commonmark.ext.gfm.strikethrough.Strikethrough
import org.commonmark.ext.gfm.strikethrough.StrikethroughExtension
import org.commonmark.node.Code
import org.commonmark.node.Emphasis
import org.commonmark.node.HardLineBreak
import org.commonmark.node.HtmlInline
import org.commonmark.node.Image
import org.commonmark.node.Link
import org.commonmark.node.Node
import org.commonmark.node.SoftLineBreak
import org.commonmark.node.StrongEmphasis
import org.commonmark.parser.Parser

val LocalNetworkImages = staticCompositionLocalOf<ImageLoader> { error("Missing image loader") }

/** 渲染 Markdown。聊天正文可显示电脑的文件路径与复制动作，图片标签仍用文本表示。 */
@Composable fun MarkdownText(source: String, selectable: Boolean = true, color: Color = Palette.text, fileLinks: Boolean = false) {
    val blocks = remember(source) { MarkdownBlocks.parse(source) }
    val context = LocalContext.current
    var selectedPath by remember(source) { mutableStateOf<String?>(null) }
    var copyFailed by remember(source) { mutableStateOf(false) }
    // 链接回调只设置当前文件动作对话框。
    val selectPath: ((String) -> Unit)? = if (fileLinks) { path -> selectedPath = path; copyFailed = false } else null
    // 预览及正文使用同样的 Markdown 块。
    val content: @Composable () -> Unit = {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) { for (block in blocks) MarkdownBlock(block, color, selectPath) }
    }
    if (selectable) SelectionContainer(content = content) else content()
    val path = selectedPath
    // 只截短显示文案，复制操作保留完整路径。
    val visiblePath = path?.let { if (it.length > 512) it.take(512) + "…" else it }
    // 手机复制原电脑路径，不用 Android 打开原文件。
    CodyncDialog("File on the host computer", path != null, { /* 关闭时清除当前路径。 */ selectedPath = null },
        message = if (copyFailed) tr("Couldn't copy the path.") else visiblePath) {
        if (path == null) emptyList() else listOf(DialogAction("Copy path") {
            // 组件会先关闭对话框，失败时恢复所选路径以便重试。
            val copied = runCatching {
                // 只写入手机剪贴板。
                context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("Codync", path))
            }.isSuccess
            Analytics.capture(AnalyticsEvent.FileLinkPathCopy(copied))
            if (!copied) { selectedPath = path; copyFailed = true }
        })
    }
}

/** 保持文件链接在段落、标题、列表、表格和引用中的行为一致。 */
@Composable private fun MarkdownBlock(block: Block, ink: Color, selectPath: ((String) -> Unit)?) {
    val code = Palette.text.copy(alpha = 0.08f)
    when (block) {
        is Block.Paragraph -> Text(inline(block.text, code, selectPath), style = Type.body, color = ink)
        is Block.Heading -> Text(inline(block.text, code, selectPath), style = when (block.level) {
            1 -> Type.title3.bold; 2 -> Type.headline; else -> Type.subheadline.bold
        }, color = ink)
        is Block.Bullet -> Row(Modifier.padding(start = 16.dp * block.depth), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(block.marker, style = Type.body.monospacedDigit, color = Palette.secondary)
            Text(inline(block.text, code, selectPath), style = Type.body, color = ink)
        }
        Block.Rule -> Box(Modifier.padding(vertical = 4.dp).fillMaxWidth().height(1.dp).background(Palette.border))
        is Block.Table -> MarkdownTable(block, code, ink, selectPath)
        is Block.Quote -> Row(Modifier.height(IntrinsicSize.Min)) {
            Box(Modifier.width(3.dp).fillMaxHeight().background(Palette.border))
            Text(inline(block.text, code, selectPath), Modifier.padding(start = 10.dp), style = Type.body, color = Palette.secondary)
        }
        is Block.Code -> Box(Modifier.fillMaxWidth().clip(RoundedRectangle(8.dp)).background(Palette.codeBackground)
            .horizontalScroll(rememberScrollState())) {
            Text(block.text, Modifier.padding(10.dp), style = Type.footnote.monospaced, color = Palette.text, softWrap = false)
        }
    }
}

/** 宽表格横向滚动，单元格保留正文链接动作。 */
@Composable private fun MarkdownTable(table: Block.Table, code: Color, ink: Color, selectPath: ((String) -> Unit)?) {
    val columns = maxOf(table.header.size, table.rows.maxOfOrNull { it.size } ?: 0)
    val rows = listOf(table.header) + table.rows
    Box(Modifier.horizontalScroll(rememberScrollState()).padding(vertical = 2.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            for (column in 0 until columns) Column(Modifier.width(IntrinsicSize.Max), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                for ((index, row) in rows.withIndex()) {
                    if (index > 0) Box(Modifier.fillMaxWidth().height(1.dp).background(Palette.border))
                    Text(inline(row.getOrElse(column) { "" }, code, selectPath), Modifier.heightIn(min = 22.dp),
                        style = if (index == 0) Type.body.semibold else Type.body, color = ink, softWrap = false)
                }
            }
        }
    }
}

private val inlineParser = Parser.builder().extensions(listOf(StrikethroughExtension.create())).build()

private val windowsPath = Regex("""^[A-Za-z]:[/\\]""")
private val localFileUrl = Regex("""^file://(?:localhost)?(?=/)""", RegexOption.IGNORE_CASE)
private val invalidPercent = Regex("""%(?![0-9a-fA-F]{2})""")

/** 解码电脑的文件链接，接受 POSIX 或 Windows 绝对路径。 */
internal fun fileLinkPath(target: String): String? {
    val fileUrl = localFileUrl.find(target)
    val encoded = if (fileUrl == null) target else target.substring(fileUrl.value.length)
    // 普通绝对路径的字面百分号是文件名的一部分。
    val malformed = invalidPercent.containsMatchIn(encoded)
    if (fileUrl != null && malformed) return null
    var path = if (malformed) encoded else Uri.decode(encoded)
    if (fileUrl != null && path.startsWith("/") && windowsPath.containsMatchIn(path.drop(1))) path = path.drop(1)
    if (path.length > 32768 || path.any { it.code < 32 || it.code == 127 }) return null
    return path.takeIf { (it.startsWith("/") && !it.startsWith("//") && !it.startsWith("/\\")) || windowsPath.containsMatchIn(it) }
}

/** 内联代码保留底色，链接以下划线区分，只有对话正文会产生文件操作。 */
internal fun inline(source: String, codeTint: Color, selectPath: ((String) -> Unit)? = null): AnnotatedString {
    // Inline only: indentation would otherwise read as a code block.
    val document = inlineParser.parse(source.lines().joinToString("\n") { it.trimStart() })
    return buildAnnotatedString {
        // 只解析 Markdown 链接节点，代码和图像标签不会触发文件动作。
        fun visit(node: Node) {
            val style = when (node) {
                is StrongEmphasis -> SpanStyle(fontWeight = FontWeight.SemiBold)
                is Emphasis -> SpanStyle(fontStyle = FontStyle.Italic)
                is Code -> SpanStyle(fontFamily = FontFamily.Monospace, background = codeTint)
                is Strikethrough -> SpanStyle(textDecoration = TextDecoration.LineThrough)
                else -> null
            }
            if (style != null) pushStyle(style)
            val target = (node as? Link)?.destination
            val path = if (selectPath != null && target != null) fileLinkPath(target) else null
            val styles = TextLinkStyles(SpanStyle(textDecoration = TextDecoration.Underline))
            val link = when {
                path != null && selectPath != null -> LinkAnnotation.Clickable(path, styles) {
                    // 点击只打开文件动作对话框。
                    selectPath(path)
                }
                target != null && (target.startsWith("https://", true) || target.startsWith("http://", true) || target.startsWith("mailto:", true)) ->
                    LinkAnnotation.Url(target, styles)
                else -> null
            }
            if (link != null) pushLink(link)
            when (node) {
                is org.commonmark.node.Text -> append(node.literal)
                is Code -> append(node.literal)
                is SoftLineBreak, is HardLineBreak -> append("\n")
                is HtmlInline -> append(node.literal)
                is org.commonmark.node.HtmlBlock -> append(node.literal.trimEnd())
                is org.commonmark.node.FencedCodeBlock -> append(node.literal.trimEnd())
                is Image -> children(node).forEach(::visit)
                else -> children(node).forEach(::visit)
            }
            if (link != null) pop()
            if (style != null) pop()
        }
        // Paragraphs of a multi-line block keep their line breaks.
        children(document).forEachIndexed { index, paragraph -> if (index > 0) append("\n\n"); visit(paragraph) }
    }
}

private fun children(node: Node): List<Node> = buildList {
    var child = node.firstChild
    while (child != null) { add(child); child = child.next }
}

/** A monospaced box for commands and output (iOS `CodeBox`). */
@Composable fun CodeBox(text: String, style: TextStyle = Type.caption.monospaced) {
    Box(Modifier.fillMaxWidth().heightIn(max = 240.dp).clip(RoundedRectangle(8.dp)).background(Palette.codeBackground)
        .horizontalScroll(rememberScrollState())) {
        SelectionContainer { Text(text, Modifier.padding(8.dp), style = style, color = Palette.text, softWrap = false) }
    }
}
