package com.codync.android

import org.junit.Assert.*
import org.junit.Test

class ComposerAttachmentTest {
    /** 超过缓存软额度的当前附件仍可读取，旧副本可淘汰，下载中的 partial 不受影响。 */
    @Test fun `a large current attachment survives cache trimming`() {
        val root = java.nio.file.Files.createTempDirectory("codync-large-cache").toFile()
        try {
            val old = root.resolve("older").apply { writeText("old"); setLastModified(1) }
            val current = root.resolve("current")
            java.io.RandomAccessFile(current, "rw").use { it.setLength(513L * 1024 * 1024) }
            val partial = root.resolve("pending.partial").apply { writeText("in progress") }
            AttachmentFiles.trim(current)
            assertTrue(current.exists())
            assertEquals(513L * 1024 * 1024, current.length())
            assertFalse(old.exists())
            assertTrue(partial.exists())
            // 下一个小文件成为当前项时，可以正常淘汰先前的大副本。
            val next = root.resolve("next").apply { writeText("next") }
            AttachmentFiles.trim(next)
            assertTrue(next.exists())
            assertFalse(current.exists())
        } finally { root.deleteRecursively() }
    }
    @Test fun `paste is offered for images and other uris, not for text`() {
        assertTrue(clipOffersFiles(listOf("image/png")))
        assertTrue(clipOffersFiles(listOf("application/pdf")))
        assertTrue(clipOffersFiles(listOf("text/uri-list")))
        assertFalse(clipOffersFiles(listOf("text/plain")))
        assertFalse(clipOffersFiles(listOf("text/plain", "text/html")))
        assertFalse(clipOffersFiles(emptyList()))
    }

    @Test fun `file chips use the symbol of the file's kind`() {
        assertEquals("photo", AttachmentIcon.symbol("shot.PNG"))
        assertEquals("doc.richtext", AttachmentIcon.symbol("paper.pdf"))
        assertEquals("doc.zipper", AttachmentIcon.symbol("logs.tar"))
        assertEquals("film", AttachmentIcon.symbol("clip.mov"))
        assertEquals("waveform", AttachmentIcon.symbol("memo.m4a"))
        assertEquals("doc", AttachmentIcon.symbol("README"))
    }
}
