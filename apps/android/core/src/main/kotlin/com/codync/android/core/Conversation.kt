package com.codync.android.core

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

val WireJson = Json { ignoreUnknownKeys = true; encodeDefaults = true }

@Serializable
data class Entry(
    val id: String,
    val seq: Long,
    val botId: String,
    val rev: Long,
    val kind: String,
    val turn: Long = 0,
    val data: JsonObject = JsonObject(emptyMap()),
    val createdAt: Long = 0,
    val updatedAt: Long = 0,
) {
    val text: String get() = data.text("text").orEmpty()
    val clientNonce: String? get() = data.text("clientNonce")
    val status: String? get() = data.text("status")
    /** Files the bot shared in this message. */
    val files: List<SharedFile> get() = data["files"]?.let { WireJson.decodeFromJsonElement<List<SharedFile>>(it) }.orEmpty()
    /** The artifact a bot published in this message (an artifact card), read leniently: its text stands in for it. */
    val artifact: ArtifactCard? get() = (data["artifact"] as? JsonObject)?.let { runCatching { WireJson.decodeFromJsonElement<ArtifactCard>(it) }.getOrNull() }
    /** A message of the user's waiting behind the bot's work: the computer hasn't given it to the agent yet. */
    val isQueued: Boolean get() = kind == "user" && status == "queued"
    /** A queued message the user took back (`unqueue`): the app's state leaves it out (`withRemovals`). */
    val isRemoved: Boolean get() = data["removed"]?.jsonPrimitive?.booleanOrNull == true
    /** Of memory and context, only a fresh conversation gets a line: memory lives in the bot's memory settings, a
     *  summary keeps the same conversation going, and a kept handoff changed nothing. */
    val isChat: Boolean get() = kind in listOf("user", "permission") ||
        kind == "notice" && "memory" !in data && "memoryFile" !in data && "compaction" !in data &&
            (data["handoff"] as? JsonObject)?.text("status") != "kept" ||
        kind == "agent" && data["final"]?.jsonPrimitive?.booleanOrNull == true
    /** A step the working indicator unfolds: thinking, narration, a tool call or a plan; never the bot's bookkeeping,
     *  nor a call the host didn't flag (one older than 2.25.0 flags none). */
    val isWorkingStep: Boolean get() = when (kind) {
        "thought" -> text.isNotBlank()
        "agent" -> data["final"]?.jsonPrimitive?.booleanOrNull != true && !isPass(text)
        "tool" -> data["bookkeeping"]?.jsonPrimitive?.booleanOrNull == false
        "plan" -> true
        else -> false
    }
    /** A message the user can quote in a reply: theirs, or a final reply (a group member's too). */
    val isReplyable: Boolean get() = kind == "user" || kind == "agent" && data["final"]?.jsonPrimitive?.booleanOrNull == true
    /** The message this one replies to, as the computer quoted it. */
    val replyTo: ReplyQuote? get() = (data["replyTo"] as? JsonObject)?.let { runCatching { WireJson.decodeFromJsonElement<ReplyQuote>(it) }.getOrNull() }
    /** What the composer shows of the message it replies to: its text, or the names of its files. */
    val quotePreview: String get() = text.ifEmpty {
        (files.map { it.name } + (data["attachments"] as? JsonArray).orEmpty().mapNotNull { (it as? JsonObject)?.text("name") }).joinToString(", ")
    }
    /** How a reply quotes this message until the computer's own quote arrives. */
    val replyQuote: ReplyQuote get() = ReplyQuote(id, if (kind == "user") "user" else data.text("author") ?: botId, quotePreview.take(500))

    companion object {
        /** No text, or a pass the host keeps out of the chat (`is_pass` in host/src/chat/group.rs). */
        private fun isPass(text: String) =
            text.trim().trim('"', '.', '*').lowercase().let { it.isEmpty() || it == "(pass)" || it == "pass" }

        /** 解析独立运行时故障与持久事件，未知事件按既有规则兼容。 */
        fun decode(value: JsonObject): Entry {
            val entry = WireJson.decodeFromJsonElement<Entry>(value)
            require(entry.id.isNotBlank() && entry.botId.isNotBlank() && entry.kind.isNotBlank() && entry.seq >= 0 && entry.rev >= 0) {
                "The computer sent an invalid conversation update."
            }
            // Validate the fields used by rendering before the mirror can checkpoint them.
            val fields = WireJson.decodeFromJsonElement<EntryFields>(entry.data)
            require(fields.callSeconds == null || fields.callSeconds >= 0)
            require(fields.options.orEmpty().all { it.optionId.isNotBlank() })
            // 用户附件没有总大小上限，仍拒绝负数和无效名称；bot 发布快照沿用自己的限制。
            require(fields.attachments.orEmpty().all { it.size >= 0 && it.name.isNotBlank() })
            return entry
        }
    }
}

// bot 发布的快照及内存备份沿用既有界限；用户发送的附件不使用此上限。
const val MAX_FILE_BYTES = 100L * 1024 * 1024

@Serializable
private data class EntryFields(
    val text: String? = null, val clientNonce: String? = null, val status: String? = null,
    val final: Boolean? = null, val title: String? = null, val command: String? = null,
    val detail: String? = null, val output: String? = null, val author: String? = null,
    val options: List<PermissionChoice>? = null, val reactions: List<String>? = null,
    val attachments: List<Attachment>? = null, val files: List<SharedFile>? = null,
    val callSeconds: Long? = null,
)
@Serializable private data class PermissionChoice(val optionId: String, val name: String, val kind: String)
/** A quoted message: its entry, who wrote it (`user` or a bot id) and its text, cut short by the computer; `artifact` when it
 *  quotes an artifact card (the message is about that artifact). */
@Serializable data class ReplyQuote(val id: String = "", val author: String = "", val text: String = "", val artifact: String? = null)

@Serializable data class Attachment(val id: String, val name: String, val size: Long) {
    val isImage: Boolean get() = isImageName(name)
}

private fun isImageName(name: String) = name.substringAfterLast('.', "").lowercase() in listOf("png", "jpg", "jpeg", "heic", "gif", "webp", "tiff", "bmp")

/** A file a bot shared: immutable on the computer, checked against its SHA-256 once downloaded. */
@Serializable data class SharedFile(val id: String, val name: String, val size: Long, val sha256: String) {
    /** An image small enough to show in the chat (the phone fetches all of it). */
    val showsAsImage: Boolean get() = isImageName(name) && size <= 20L * 1024 * 1024

    /** Before any of it is read: a name that can't leave its folder and a hash to check. */
    fun validate() {
        require(size in 0..MAX_FILE_BYTES && sha256.length == 64 && sha256.all { it in "0123456789abcdef" } &&
            name.isNotEmpty() && name.toByteArray(Charsets.UTF_8).size <= 200 && name != "." && name != ".." &&
            '/' !in name && '\\' !in name && name.codePoints().noneMatch { Character.getType(it).toByte() in CONTROLS }) { "Invalid file metadata." }
    }
}

/** Unicode control and format characters, which a file name may not contain. */
private val CONTROLS = setOf(Character.CONTROL, Character.FORMAT)

/** [Waiting]: in the relay's mailbox. [Held]: on this phone, until the computer can be reached. */
@Serializable
enum class SendStatus { Sending, Waiting, Delivering, Failed, Held }

@Serializable
data class PendingSend(
    val nonce: String,
    val botId: String,
    val text: String,
    val createdAt: Long,
    val status: SendStatus = SendStatus.Sending,
    val error: String? = null,
    val files: List<OutgoingFile> = emptyList(),
    /** The message it quotes (`send`'s `replyTo` is its id), shown on the bubble until the computer has it. */
    val replyTo: ReplyQuote? = null,
    /** The voice call it was said in (on-device or speech to text). */
    val call: String? = null,
    val mentions: List<Mention> = emptyList(),
    val draftKey: String? = null,
) {
    /** The `send` request for it, with the ids of its uploaded files. */
    /** 构造实际发送正文，原始 UTF-16 提及范围与附件引用一起传输。 */
    fun body(attachments: List<String> = emptyList()): JsonObject = buildJsonObject {
        put("botId", botId); put("text", text); put("clientNonce", nonce)
        put("mentions", WireJson.encodeToJsonElement(kotlinx.serialization.builtins.ListSerializer(Mention.serializer()), mentions))
        replyTo?.let { put("replyTo", it.id) }
        call?.let { put("call", it) }
        if (attachments.isNotEmpty()) put("attachments", JsonArray(attachments.map(::JsonPrimitive)))
    }
}

@Serializable
data class OutgoingFile(val path: String, val name: String, val size: Long)

sealed interface MirrorEvent {
    data class Icons(val icons: Map<String, CustomIcon>, val revision: Long = 0) : MirrorEvent
    /** The stream's first event; `network` is null from hosts before the network view (or one this app can't read). */
    data class Hello(val hostId: String, val rev: Long, val usage: JsonObject, val screen: JsonObject?, val network: NetworkView? = null) : MirrorEvent
    data class DeliveryStorageError(val botId: String, val error: String?, val status: String, val activity: String, val workingChat: String?, val pendingNewSession: Boolean) : MirrorEvent
    data class BotChanged(val bot: Bot) : MirrorEvent
    data class EntryChanged(val entry: Entry) : MirrorEvent
    data class Usage(val value: JsonObject) : MirrorEvent
    data class Screen(val value: JsonObject) : MirrorEvent
    /** An artifact was published, updated or renamed (the catch-up replays it too), never a checkpoint. */
    data class ArtifactChanged(val artifact: Artifact) : MirrorEvent
    /** For the voice of a call on this device: live only, never a checkpoint. */
    data class Call(val cue: CallCue) : MirrorEvent
    /** The computer's network changed: live only, never a checkpoint. */
    data class Network(val view: NetworkView) : MirrorEvent
    data object Resync : MirrorEvent
    data object Ignored : MirrorEvent

    companion object {
        fun decode(value: JsonObject): MirrorEvent = when (value.text("type")) {
            "icons" -> Icons(decodeCustomIcons(value.getValue("icons")), value["iconsRevision"]?.jsonPrimitive?.longOrNull ?: 0)
            "hello" -> Hello(requireNotNull(value.text("hostId")),
                requireNotNull(value["rev"]?.jsonPrimitive?.longOrNull),
                value["usage"]?.jsonObject ?: JsonObject(emptyMap()), value["screen"]?.takeUnless { it == JsonNull }?.jsonObject,
                value["network"]?.takeUnless { it == JsonNull }?.let { runCatching { NetworkView.decode(it) }.getOrNull() })
            "deliveryStorageError" -> DeliveryStorageError(requireNotNull(value.text("botId")), value.text("storageError"), value.text("status") ?: "idle", value.text("activity").orEmpty(), value.text("workingChat"), value["pendingNewSession"]?.jsonPrimitive?.booleanOrNull == true)
            "bot" -> BotChanged(HostClient.decode(value.getValue("bot").jsonObject))
            "entry" -> EntryChanged(Entry.decode(value.getValue("entry").jsonObject))
            "usage" -> Usage(value.getValue("usage").jsonObject)
            "screen" -> Screen(value.getValue("screen").jsonObject)
            // One this app can't read is passed over rather than fetching everything again: a card or the Library reads it again.
            "artifact" -> runCatching { ArtifactChanged(Artifact.decode(value.getValue("artifact"))) }.getOrDefault(Ignored)
            "call" -> CallCue.decode(value)?.let(::Call) ?: Ignored
            // One this app can't read is passed over: the next stream's hello carries the view again.
            "network" -> runCatching { Network(NetworkView.decode(value.getValue("network"))) }.getOrDefault(Ignored)
            "resync" -> Resync
            // Match the existing clients: unknown events do not advance the cursor.
            else -> Ignored
        }
    }
}

data class MirrorSnapshot(
    val bots: List<Bot> = emptyList(),
    val entries: List<Entry> = emptyList(),
    val pending: List<PendingSend> = emptyList(),
    val rev: Long = 0,
    val usage: JsonObject = JsonObject(emptyMap()),
    val screen: JsonObject? = null,
    val icons: Map<String, CustomIcon> = emptyMap(),
)

/** 固定成员身份，偏移始终采用实际请求正文的 UTF-16 半开区间。 */
@Serializable
data class Mention(val botId: String, val start: Int, val end: Int)

/** 片段外的编辑平移绑定；触及片段正文时解除绑定，避免改名误投。 */
fun editMentions(before: String, after: String, mentions: List<Mention>, replacement: IntRange? = null): List<Mention> {
    if (replacement != null) {
        val delta = after.length - before.length
        // 原生编辑范围只移动外部绑定，修改显示片段必须解除旧身份。
        return mentions.mapNotNull { mention ->
            val extendsName = mention.end == replacement.first && delta > 0 && (mention.end in after.indices && after.codePointAt(mention.end).let { Character.isLetterOrDigit(it) || it == '_'.code })
            when {
                mention.end <= replacement.first && !extendsName -> mention
                mention.start >= replacement.last + 1 -> mention.copy(start = mention.start + delta, end = mention.end + delta)
                else -> null
            }
        }
    }
    if (before == after) return mentions
    var start = 0
    var suffix = 0
    while (start < before.length && start < after.length && before[start] == after[start]) start++
    while (suffix < before.length && suffix < after.length && before[before.length - suffix - 1] == after[after.length - suffix - 1]) suffix++
    // 重复字符有多个可能的编辑范围，保守解除可能被触及的身份。
    val removed = maxOf(0, before.length - after.length, before.length - start - suffix)
    val earliest = before.length - suffix - removed
    val latestEnd = start + removed
    val delta = after.length - before.length
    return mentions.mapNotNull { mention -> when {
        mention.end <= earliest && !(mention.end == earliest && delta > 0 && (mention.end in after.indices && after.codePointAt(mention.end).let { Character.isLetterOrDigit(it) || it == '_'.code })) -> mention
        mention.start >= latestEnd -> mention.copy(start = mention.start + delta, end = mention.end + delta)
        else -> null
    } }
}

/** 聚合卡内的目标行，当前显示名从成员列表读取。 */
@Serializable
data class DeliveryRow(val deliveryId: String, val targetBotId: String, val status: String, val detail: String = "") {
    /** 状态使用中文展示，未知协议值保留稳定兜底。 */
    val statusText: String get() = when (status) {
        "queued" -> "等待执行"
        "claimed" -> "准备执行"
        "started" -> "执行中"
        "completed" -> "已完成"
        "failed" -> "失败"
        "interrupted" -> "已中断"
        "noReply" -> "未回复"
        "cancelled" -> "已停止"
        else -> "状态待更新"
    }

    companion object {
        /** 一行聚合当前目标，仅全部停止时才宣称自动接力停止。 */
        fun summary(rows: List<DeliveryRow>): String {
            val first = rows.firstOrNull() ?: return "群内派工状态待更新"
            if (rows.all { it.status == "cancelled" && it.detail == first.detail }) {
                when (first.detail) {
                    "对话轮次上限" -> return "自动接力已达 3 轮，已停止继续派工"
                    "群派工链深上限" -> return "自动接力已达链深上限，已停止继续派工"
                    "房间发言上限" -> return "本轮发言已达上限，已停止继续派工"
                }
            }
            val counts = linkedMapOf<String, Int>()
            for (row in rows) counts[row.statusText] = (counts[row.statusText] ?: 0) + 1
            if (counts.size == 1) return "群内派工${first.statusText}"
            return "群内派工：" + counts.entries.joinToString("，") { "${it.value} 项${it.key}" }
        }
    }
}
