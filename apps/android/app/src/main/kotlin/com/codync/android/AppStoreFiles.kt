package com.codync.android

import android.content.ClipData
import android.content.ClipboardManager
import android.net.Uri
import android.provider.DocumentsContract
import android.util.Log
import com.codync.android.core.*
import com.codync.android.AnalyticsEvent.FileAction
import com.codync.android.AnalyticsEvent.FileActionOutcome
import java.io.File
import java.nio.ByteBuffer
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*

// Files a bot shares in a chat (iOS `FileDownloads`): one download at a time, checked against its
// SHA-256, then handed to the system share sheet.

/** Where a shared file's download is, as iOS `FileDownloads.State`. */
sealed interface FileDownloadState {
    data class Downloading(val received: Long) : FileDownloadState
    data object Saved : FileDownloadState
    data class Failed(val message: String) : FileDownloadState
}

internal sealed class SharedFileAction(val analytics: FileAction) {
    data object Share : SharedFileAction(FileAction.Save)
    data object Open : SharedFileAction(FileAction.Open)
    data object OpenWith : SharedFileAction(FileAction.Open)
    data object CopyContents : SharedFileAction(FileAction.CopyContents)
    class SaveAs(val pickDestination: (File) -> Unit) : SharedFileAction(FileAction.Save)
}

// Binder carries clipboard text as UTF-16, alongside the rest of the parcel.
private const val TEXT_COPY_LIMIT = 256 * 1024
private const val TEXT_COPY_FAILURE = "Copy file contents supports UTF-8 text files up to 256 KiB."

private fun downloadKey(fileId: String) = "file:$fileId"

/** Downloads the chat snapshot and hands it to the selected device action. */
internal fun AppStore.saveFile(entryId: String, botId: String, file: SharedFile, use: SharedFileAction = SharedFileAction.Share) {
    val running = state.value.busy.filter { it.startsWith("file:") }
    if (running.isNotEmpty()) {
        if (downloadKey(file.id) !in running) setDownload(file.id, FileDownloadState.Failed("Finish the current download first."))
        return
    }
    if (use == SharedFileAction.CopyContents && file.size > TEXT_COPY_LIMIT) {
        setDownload(file.id, FileDownloadState.Failed(TEXT_COPY_FAILURE))
        report(use.analytics, FileActionOutcome.Unsupported)
        return
    }
    action(downloadKey(file.id)) {
        var temporary: File? = null
        var handedOff = false
        try {
            setDownload(file.id, FileDownloadState.Downloading(0))
            val folder = withContext(Dispatchers.IO) { attachments.shareFolder() }
            temporary = folder
            val saved = HostFiles(ready()).downloadShared(entryId, file, folder) { setDownload(file.id, FileDownloadState.Downloading(it)) }
            if (!showing(botId)) {
                setDownload(file.id, null)
                report(use.analytics, FileActionOutcome.Cancelled)
                return@action
            }
            when (use) {
                SharedFileAction.Share -> attachments.open(saved, file.name)
                SharedFileAction.Open, SharedFileAction.OpenWith -> attachments.view(saved, choose = use == SharedFileAction.OpenWith)
                is SharedFileAction.SaveAs -> use.pickDestination(saved)
                SharedFileAction.CopyContents -> {
                    val text = withContext(Dispatchers.IO) { readSharedText(saved) }
                    application.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("Codync", text))
                }
            }
            handedOff = use != SharedFileAction.CopyContents
            setDownload(file.id, if (use == SharedFileAction.Share || use == SharedFileAction.Open || use == SharedFileAction.OpenWith) {
                FileDownloadState.Saved
            } else null)
            if (use !is SharedFileAction.SaveAs) report(use.analytics, FileActionOutcome.Ok)
        } catch (cancelled: CancellationException) {
            setDownload(file.id, null)
            report(use.analytics, FileActionOutcome.Cancelled)
            throw cancelled
        } catch (invalid: InvalidSharedText) {
            setDownload(file.id, FileDownloadState.Failed(TEXT_COPY_FAILURE))
            report(use.analytics, FileActionOutcome.Unsupported)
        } catch (error: Exception) {
            setDownload(file.id, FileDownloadState.Failed(error.message ?: "Couldn't download the file."))
            report(use.analytics, FileActionOutcome.Failed)
        } finally {
            if (!handedOff) withContext(NonCancellable + Dispatchers.IO) { temporary?.deleteRecursively() }
        }
    }
}

fun AppStore.cancelFile(fileId: String) {
    actions[downloadKey(fileId)]?.cancel()
    setDownload(fileId, null)
}

private fun AppStore.setDownload(fileId: String, download: FileDownloadState?) = mutableState.update {
    it.copy(fileDownloads = if (download == null) it.fileDownloads - fileId else it.fileDownloads + (fileId to download))
}

/** The chat is the one open. */
private fun AppStore.showing(botId: String): Boolean = foreground && state.value.selectedBot == botId

internal fun AppStore.exportSharedFile(fileId: String, source: File, target: Uri?) {
    action("export:$fileId") {
        try {
            if (target != null) withContext(Dispatchers.IO) {
                val resolver = application.contentResolver
                try {
                    val output = requireNotNull(resolver.openOutputStream(target, "wt"))
                    output.use { destination -> source.inputStream().use { it.copyTo(destination) } }
                } catch (error: Exception) {
                    // The create-document picker returned a new document. Remove incomplete exports
                    // when the provider supports deletion, and preserve the original write error.
                    try { DocumentsContract.deleteDocument(resolver, target) }
                    catch (cleanup: Exception) { Log.w("Codync", "Couldn't remove an incomplete file export", cleanup) }
                    throw error
                }
            }
            setDownload(fileId, if (target == null) null else FileDownloadState.Saved)
            report(FileAction.Save, if (target == null) FileActionOutcome.Cancelled else FileActionOutcome.Ok)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            setDownload(fileId, FileDownloadState.Failed(error.message ?: "Could not complete the file action."))
            report(FileAction.Save, FileActionOutcome.Failed)
        } finally {
            withContext(NonCancellable + Dispatchers.IO) { source.parentFile?.parentFile?.deleteRecursively() }
        }
    }
}

private class InvalidSharedText : Exception()

private fun readSharedText(file: File): String {
    val text = try {
        Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(file.readBytes())).toString().removePrefix("\uFEFF")
    } catch (_: java.nio.charset.CharacterCodingException) { throw InvalidSharedText() }
    if (text.any { it in '\u0000'..'\u0008' || it in '\u000B'..'\u000C' || it in '\u000E'..'\u001F' || it == '\u007F' }) throw InvalidSharedText()
    return text
}

private fun report(action: FileAction, outcome: FileActionOutcome) =
    Analytics.capture(AnalyticsEvent.SharedFileAction(action, outcome))
