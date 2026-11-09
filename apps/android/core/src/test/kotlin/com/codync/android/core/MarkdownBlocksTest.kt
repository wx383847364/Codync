package com.codync.android.core

import com.codync.android.core.MarkdownBlocks.Block
import org.junit.Assert.assertEquals
import org.junit.Test

class MarkdownBlocksTest {
    /** Tables, rules and nested lists, as agents write them (the iOS test's case). */
    @Test fun `tables rules and nesting split like iOS`() {
        val blocks = MarkdownBlocks.parse("| Name | Size |\n|---|:-:|\n| a | 1 |\n---\n- top\n  - inner\n| not a table")
        assertEquals(listOf(Block.Table(listOf("Name", "Size"), listOf(listOf("a", "1"))), Block.Rule,
            Block.Bullet("top", "•", 0), Block.Bullet("inner", "◦", 1), Block.Paragraph("| not a table")), blocks)
    }

    @Test fun `headings quotes ordered items and fences`() {
        val blocks = MarkdownBlocks.parse("## Plan\n1. read\n> note\n```kotlin\nval x = 1\n```\ntext\nmore")
        assertEquals(listOf(Block.Heading("Plan", 2), Block.Bullet("read", "1.", 0), Block.Quote("note"),
            Block.Code("val x = 1", "kotlin"), Block.Paragraph("text\nmore")), blocks)
    }
    /** 围栏内的文件链接只是示例文字，较短围栏不会提前闭合。 */
    @Test fun `file link examples stay inside tilde and longer backtick fences`() {
        val example = "[file](/tmp/example.png)"
        val source = listOf("~~~md", example, "~~~", "````md", "```", example, "```", "````").joinToString("\n")
        assertEquals(listOf(Block.Code(example, "md"), Block.Code("```\n$example\n```", "md")), MarkdownBlocks.parse(source))
    }
}
