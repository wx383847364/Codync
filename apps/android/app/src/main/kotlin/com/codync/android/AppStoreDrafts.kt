package com.codync.android

import android.net.Uri
import java.io.File
import java.nio.ByteBuffer
import java.nio.CharBuffer
import java.nio.charset.CodingErrorAction
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.sync.withLock
import com.codync.android.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*

// Composer drafts (saved as you type) and the files that go with them.

/** 编辑草稿时同步维护身份，避免离线或重试按名字重新派发。 */
fun AppStore.editDraft(botId: String, text: String, replacement: IntRange? = null) {
    if (state.value.loading) return
    val current = state.value.drafts[botId]
    if (current?.text == text || current == null && text.isEmpty()) return
    try { updateDraft((current ?: ComposerDraft(botId)).copy(text = text, mentions = editMentions(current?.text.orEmpty(), text, current?.mentions.orEmpty(), replacement))) }
    catch (error: Exception) { showError(error) }
}

/** 一次保存原文和引用身份，编辑范围之后的既有引用同步平移。 */
fun AppStore.bindMention(key: String, text: String, mention: Mention, replacement: IntRange? = null): Boolean {
    if (state.value.loading || draftBook == null) return false
    val current = state.value.drafts[key] ?: ComposerDraft(key)
    val shifted = editMentions(current.text, text, current.mentions, replacement)
    return try {
        updateDraft(current.copy(text = text, mentions = shifted + mention))
        true
    } catch (error: Exception) {
        // 超出草稿限额时保持原正文和引用，输入框不能先行更新。
        showError(error)
        false
    }
}

/** 搜索目标聊天在当前 Host 上的工作区，结果不读取手机本地文件。 */
internal suspend fun AppStore.searchWorkspaceFiles(botId: String, query: String): List<WorkspaceFileCandidate> {
    val response = hostCall("searchWorkspaceFiles", body("botId" to botId, "query" to query))
    return WireJson.decodeFromJsonElement(response.jsonObject.getValue("items"))
}

/** host 拒发后恢复原目标草稿，不覆盖随后输入的新内容。 */
internal fun AppStore.restoreFailedDraft(send: PendingSend) {
    val key = send.draftKey ?: send.botId
    if (state.value.drafts[key]?.text.isNullOrEmpty()) updateDraft(ComposerDraft(key, send.text, send.files, send.mentions, send.nonce))
}

/** Puts [text] after what's in [botId]'s message box (text shared from another app), as much of it as the box holds. */
fun AppStore.appendDraft(botId: String, text: String) = editDraft(botId, sharedDraft(state.value.drafts[botId]?.text.orEmpty(), text))

/** [shared] after [before], cut at a character's edge to the most a draft holds (`DraftBook.TEXT_BYTES` of UTF-8). */
internal fun sharedDraft(before: String, shared: String): String {
    val joined = DictationText.join(before, shared)
    val input = CharBuffer.wrap(joined)
    // The encoder stops before the first character that doesn't fit, never inside one; a lone surrogate counts as the '?' it's saved as.
    Charsets.UTF_8.newEncoder().onMalformedInput(CodingErrorAction.REPLACE).encode(input, ByteBuffer.allocate(DraftBook.TEXT_BYTES), true)
    return joined.substring(0, input.position())
}

fun AppStore.addDraftFiles(botId: String, files: List<OutgoingFile>) {
    check(!state.value.loading) { "Wait for the account to finish opening before adding files." }
    val current = state.value.drafts[botId] ?: ComposerDraft(botId)
    updateDraft(current.copy(files = current.files + files))
}

fun AppStore.removeDraftFile(botId: String, file: OutgoingFile) = action("draft-file:" + file.path) {
    val current = state.value.drafts[botId] ?: return@action
    updateDraft(current.copy(files = current.files - file))
    draftWriter?.join()
    check(savedDrafts.values.none { draft -> draft.files.any { it.path == file.path } }) { "Couldn't save the attachment removal. Retry before leaving this chat." }
    if (state.value.pending.none { send -> send.files.any { it.path == file.path } }) withContext(Dispatchers.IO) { attachments.discard(file) }
}

/** 修改已尝试发送的正文、附件或身份属于新意图，不能复用旧请求的幂等 nonce。 */
private fun AppStore.updateDraft(draft: ComposerDraft) {
    val book = draftBook ?: return
    val previous = state.value.drafts[draft.botId]
    val represented = state.value.pending.any { it.nonce == draft.nonce } || state.value.entries.any { it.clientNonce == draft.nonce }
    val saving = draft.forSaving(previous, represented)
    val drafts = if (saving.text.isEmpty() && saving.files.isEmpty()) state.value.drafts - saving.botId else state.value.drafts + (saving.botId to saving)
    book.validate(drafts.values, checkPaths = false)
    mutableState.update { it.copy(drafts = drafts) }
    persistDrafts()
}

internal fun AppStore.persistDrafts() {
    if (draftWriter?.isActive == true) return
    val book = draftBook ?: return
    draftWriter = contextScope.launch {
        try {
            while (isActive) {
                val saving = state.value.drafts.values.toList()
                withContext(Dispatchers.IO) { book.save(saving) }
                savedDrafts = saving.associateBy { it.botId }
                if (state.value.drafts.values.toList() == saving) break
            }
        } catch (error: CancellationException) { throw error }
        catch (_: Exception) { mutableState.update { it.copy(error = "Couldn't save this draft. Keep the app open and retry.") } }
        finally { draftWriter = null }
    }
}

/** Sends draft `key` (the chat's, or one of its artifacts') to chat `botId`, quoting `replyTo` when there is one. */
/** 先持久化草稿和 nonce，再写待发送记录，身份随同一快照传递。 */
fun AppStore.sendDraft(key: String, botId: String, text: String, replyTo: ReplyQuote?, onQueued: () -> Unit) {
    editDraft(key, text)
    val draft = state.value.drafts[key] ?: return
    if (draft.text != text) return
    if (draft.text.isBlank() && draft.files.isEmpty()) return
    action("draft-send:$key") {
        // Persist its nonce before the pending commit. Recovery removes a draft
        // already represented by a pending or confirmed send with this nonce.
        persistDrafts()
        draftWriter?.join()
        check(savedDrafts[key] == draft) { "Couldn't save this draft. Retry to send it." }
        val send = PendingSend(draft.nonce, botId, draft.text, System.currentTimeMillis(), files = draft.files, replyTo = replyTo, mentions = draft.mentions, draftKey = key)
        lastSentNonce = send.nonce
        savePending(send)
        mutableState.update { it.copy(drafts = it.drafts - key) }
        persistDrafts()
        onQueued()
        action("send:" + send.nonce) { deliver(send) }
    }
}

fun AppStore.importFiles(uris: List<Uri>, onImported: (List<OutgoingFile>) -> Unit) = action("attachments") {
    val imported = mutableListOf<OutgoingFile>()
    try {
        for (uri in uris) {
            require(uri.scheme == "content") { "Choose a file using Photos or Files." }
            imported.add(attachments.import(uri))
        }
        onImported(imported.toList())
    } catch (error: Throwable) {
        withContext(NonCancellable + Dispatchers.IO) { imported.forEach(attachments::discard) }
        throw error
    }
}
fun AppStore.removeFile(file: OutgoingFile) = action("remove:" + file.path) { withContext(Dispatchers.IO) { attachments.discard(file) } }

suspend fun AppStore.attachment(botId: String, file: Attachment): File =
    cachedFile(file.id, file.size) { files, destination -> files.download(botId, file, destination) }

/** A picture a bot shared (iOS `sharedFileData`): checked against its SHA-256 on its way into the same cache. */
suspend fun AppStore.sharedImage(entryId: String, file: SharedFile): File = cachedFile(file.id, file.size) { files, destination ->
    val folder = withContext(Dispatchers.IO) { attachments.shareFolder() }
    val saved = files.downloadShared(entryId, file, folder)
    withContext(Dispatchers.IO) { try { saved.copyTo(destination, overwrite = true) } finally { folder.deleteRecursively() } }
}

/** 缓存来自电脑的文件；回收其他旧副本时保留当前文件，允许大附件突破缓存软额度。 */
private suspend fun AppStore.cachedFile(id: String, size: Long, fetch: suspend (HostFiles, File) -> Unit): File {
    // 同一文件合并请求，全电脑最多两项下载；账号退场取消所属 scope。
    val request = contextScope.async { downloadLocks.getOrPut(id) { Mutex() }.withLock { attachmentReads.withPermit {
    val destination = withContext(Dispatchers.IO) { attachments.cached(id) }
    if (!destination.exists() || destination.length() != size) {
        fetch(HostFiles(ready()), destination)
        withContext(Dispatchers.IO) { AttachmentFiles.trim(destination) }
    }
    destination
    } } }
    try { return request.await() } finally { request.cancel() }
}
