package com.codync.android.core

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*

/** 阶段群的负责人、确认材料和准入状态由 Host 保存。 */
@Serializable data class StageGroup(val leaderId: String, val goal: String, val acceptance: String,
    val handoff: StageHandoff? = null, val incoming: StageFile? = null, val closedAt: Long? = null)

@Serializable data class StageFile(val id: String, val name: String, val sha256: String, val size: Long)
@Serializable data class StageHandoff(val file: StageFile, val evidence: String, val acceptedAt: Long? = null)
@Serializable data class StageDetails(val bot: Bot, val markdown: String?, val incomingMarkdown: String?,
    val handoffUnavailable: Boolean, val incomingUnavailable: Boolean, val template: String, val initialDraft: String, val work: Work,
    val sharedFiles: List<StageFile> = emptyList()) {
    @Serializable data class Work(val counts: Map<String, Int>, val pending: List<Pending>)
    @Serializable data class Pending(val id: String, val name: String, val status: String)
}

/** 将预览时的源角色版本和用户确认的交接版本绑定到创建请求。 */
@Serializable data class StageGroupDraft(val botId: String, val requestId: String, val name: String, val goal: String,
    val acceptance: String, val leaderId: String, val roles: List<Role>, val handoffVersion: String? = null) {
    @Serializable data class Role(val botId: String, val sourceRev: Long, val description: String, val reuseProject: Boolean = false)
}

/** 操作只传各自需要的字段，Host 核验具体版本和可操作状态。 */
@Serializable data class StageChange(val botId: String, val action: String, val markdown: String? = null,
    val evidence: String? = null, val previousVersion: String? = null, val version: String? = null)

/** 缺报量保持 null，并记录每个字段覆盖了多少次尝试。 */
@Serializable data class StageUsage(val observedSince: Long, val stageCreatedAt: Long, val sections: List<Section>) {
    @Serializable data class Section(val label: String, val backend: String? = null, val model: String? = null, val rows: List<Row>)
    @Serializable data class Row(val label: String, val value: Long?, val reports: Long? = null, val attempts: Long? = null)
}

/** A group uses the existing bot endpoints; it never owns an agent or workspace. */
data class GroupDraft(
    val id: String? = null,
    val name: String = "",
    val description: String = "",
    val members: List<String> = emptyList(),
    val pinned: Boolean = false,
    val groupLeaderId: String? = null,
) {
    /** 校验成员并编码普通建群的可选负责人。 */
    fun body(roster: List<Bot>): JsonObject {
        val picked = members.distinct().map { id ->
            requireNotNull(roster.firstOrNull { it.id == id && !it.deleted && it.kind != "group" }) {
                "A member is no longer available. Remove it before saving."
            }
        }
        require(picked.isNotEmpty()) { "Choose at least one bot." }
        return buildJsonObject {
            id?.let { put("id", it) }
            put("kind", "group")
            put("name", name.trim().ifEmpty { picked.joinToString(", ") { it.name } })
            put("description", description.trim())
            put("members", JsonArray(picked.map { JsonPrimitive(it.id) }))
            put("pinned", pinned)
            if (id == null) groupLeaderId?.let { put("groupLeaderId", it) }
        }
    }

    companion object {
        fun from(bot: Bot): GroupDraft {
            require(bot.kind == "group")
            return GroupDraft(bot.id, bot.name, bot.description, bot.members, bot.pinned)
        }
    }
}

