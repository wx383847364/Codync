package com.codync.android

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.OpenableColumns
import androidx.core.content.FileProvider
import com.codync.android.core.*
import java.io.File
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlin.coroutines.coroutineContext

/** Copies provider streams into this account's private storage before it owns a send. */
class AttachmentFiles(private val context: Context, private val directory: File) {
    private val outgoing = outgoingFolder(directory).apply { mkdirs() }
    private val downloads = File(directory, "attachments").apply { mkdirs() }

    /** 流式复制选中的完整附件；总大小由可用存储决定，失败时删除半成品。 */
    suspend fun import(uri: Uri): OutgoingFile = withContext(Dispatchers.IO) {
        check(outgoing.mkdirs() || outgoing.isDirectory)
        var name = "file"
        // 仅查询名称，provider 报告的大小不再用于拒绝视频或其他附件。
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                name = cursor.getString(0) ?: name
            }
        }
        name = cleanName(name)
        require(outgoing.listFiles().orEmpty().size < 200) {
            "Attachment storage is full. Send or remove older files first."
        }
        val destination = File(outgoing, UUID.randomUUID().toString())
        try {
            val input = requireNotNull(context.contentResolver.openInputStream(uri)) { "Couldn't open the selected file." }
            input.use { source -> destination.outputStream().use { target ->
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    coroutineContext.ensureActive()
                    val count = source.read(buffer)
                    if (count < 0) break
                    target.write(buffer, 0, count)
                }
                target.fd.sync()
            } }
            if (name.substringAfterLast('.', "").lowercase() in listOf("heic", "heif", "bmp")) {
                val bitmap = thumbnail(destination, 4096)
                val converted = File(outgoing, UUID.randomUUID().toString())
                try {
                    converted.outputStream().use { target -> check(bitmap.compress(Bitmap.CompressFormat.JPEG, 85, target)); target.fd.sync() }
                    destination.delete()
                    return@withContext OutgoingFile(converted.path, name.substringBeforeLast('.') + ".jpg", converted.length())
                } catch (error: Exception) { converted.delete(); throw error }
            }
            OutgoingFile(destination.path, name, destination.length())
        } catch (error: Throwable) { destination.delete(); throw error }
    }

    fun discard(file: OutgoingFile) {
        if (inOutgoing(directory, file.path)) File(file.path).delete()
    }
    fun retainPending(files: List<OutgoingFile>) {
        val paths = files.map { File(it.path).canonicalPath }.toSet()
        outgoing.listFiles().orEmpty().filter { it.canonicalPath !in paths }.forEach { it.delete() }
    }
    fun cached(id: String): File {
        check(downloads.mkdirs() || downloads.isDirectory)
        val uuid = UUID.fromString(id).toString()
        require(uuid == id.lowercase()) { "Invalid attachment ID." }
        return File(downloads, uuid)
    }
    /** 发送后缓存本地副本，裁剪其他文件时保护当前副本，避免大文件立即被删。 */
    fun cache(id: String, source: OutgoingFile) {
        val destination = cached(id)
        File(source.path).copyTo(destination, overwrite = true)
        trim(destination)
    }

    /**
     * A new folder for one file the share sheet hands out. The receiving app reads it after the sheet
     * closes, so folders are cleared only once they're a day old.
     */
    fun shareFolder(): File {
        val root = File(context.cacheDir, "shared-attachments").apply { mkdirs() }
        root.listFiles().orEmpty().filter { System.currentTimeMillis() - it.lastModified() > 24 * 60 * 60 * 1000 }.forEach { it.deleteRecursively() }
        return File(root, UUID.randomUUID().toString())
    }

    /** The system share sheet for a file in a `shareFolder`. */
    fun open(shared: File, name: String) {
        val intent = Intent(Intent.ACTION_SEND).setType(mimeType(shared)).putExtra(Intent.EXTRA_STREAM, shareUri(shared))
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        context.startActivity(Intent.createChooser(intent, "Share $name").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    fun view(shared: File, choose: Boolean) {
        val intent = Intent(Intent.ACTION_VIEW).setDataAndType(shareUri(shared), mimeType(shared))
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        val target = if (choose) Intent.createChooser(intent, null) else intent
        context.startActivity(target.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    private fun shareUri(shared: File): Uri = FileProvider.getUriForFile(context, context.packageName + ".files", shared)

    companion object {
        fun mimeType(shared: File): String =
            android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(shared.extension.lowercase()) ?: "application/octet-stream"

        /** 按缓存软额度淘汰旧文件，但当前即将使用的文件即使单独超额也必须保留。 */
        fun trim(keeping: File) {
            val directory = keeping.parentFile ?: return
            // 正在写入的 partial 不参与淘汰；当前文件的路径由私有缓存生成。
            val files = directory.listFiles().orEmpty().filter { it.isFile && !it.name.endsWith(".partial") }.sortedBy { it.lastModified() }
            var bytes = files.sumOf { it.length() }
            for (file in files) if (file != keeping && bytes > 512L * 1024 * 1024) {
                val size = file.length()
                if (file.delete()) bytes -= size
            }
        }

        /** Where account folder [directory] keeps the files its drafts and queued messages send. */
        fun outgoingFolder(directory: File) = File(directory, "outgoing")
        fun inOutgoing(directory: File, path: String) = File(path).canonicalFile.parentFile == outgoingFolder(directory).canonicalFile

        /** [file] saved in account folder [from], pointed at its copy in [to] (`LocalStore.adopt`); others stay as they are. */
        fun moved(file: OutgoingFile, from: File, to: File): OutgoingFile =
            if (inOutgoing(from, file.path)) file.copy(path = File(outgoingFolder(to), File(file.path).name).path) else file

        fun cleanName(value: String): String {
            val name = value.substringAfterLast('/').substringAfterLast('\\').trim().trimStart('.').filter { !it.isISOControl() }
            val usable = name.ifBlank { "file" }
            require(usable.toByteArray(Charsets.UTF_8).size <= 200) { "The file name is too long." }
            return usable
        }
        /** 解码前检查原图像素上限，预览失败仍允许下载原文件。 */
        fun thumbnail(file: File, maxPixels: Int): Bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(file)) { decoder, info, _ ->
            if (maxPixels > 4096) require(file.length() <= 20L * 1024 * 1024 && info.size.width <= 32768 && info.size.height <= 32768 &&
                info.size.width.toLong() * info.size.height <= 32_000_000L) { "Image exceeds the preview pixel limit." }
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            val longest = maxOf(info.size.width, info.size.height)
            decoder.setTargetSampleSize(maxOf(1, (longest + maxPixels - 1) / maxPixels))
        }
    }
}
