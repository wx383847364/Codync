import Foundation

public struct EntryData: Codable, Hashable, Sendable {
    public var deliveryAttempt: Int?
    public var parentDeliveryId: String?
    public var deliveryId: String?
    public var deliveryBody: String?
    public var deliveryRows: [DeliveryRow]?
    public var mentions: [Mention]?
    public var connectionRequest: ConnectionRequest?
    /// A memory line an older host put in the chat. Memory lives in the bot's memory settings, so it isn't shown.
    public var memory: MemoryLine?
    public var memoryFile: MemoryLine?
    /// notice: the agent summarized its context.
    public var compaction: Compaction?
    /// notice: the bot's chat starts over with this handoff.
    public var handoff: HandoffCard?
    public var routineId: String?
    public var runId: String?
    /// notice: a failed routine run the user can run again (`retryRoutineRun`).
    public var retry: Bool?
    /// notice: the bot moved to another agent of its chain, or back to its own.
    public var agentSwitch: AgentSwitch?
    /// notice (error): the bot has no fallbacks and its agent ran out of usage or was rate limited: offer to add one.
    public var addFallback: Bool?
    public var text: String?
    public var final: Bool?
    /// user: queued | sent | cancelled | failed, and before the computer has it: sending | waiting (in the relay) | held (on this phone) | delivering · tool: pending | in_progress | completed | failed · permission: pending | answered | cancelled | expired
    public var status: String?
    /// user: taken back while it waited (`unqueue`): it never went out, and no view reads it (`BotStore.chat`).
    public var removed: Bool?
    public var clientNonce: String?
    public var title: String?
    public var toolKind: String?
    /// tool: whether it's the bot's own bookkeeping (composing its message, keeping its memory), left out of the
    /// working steps; `nil` from a host older than 2.25.0.
    public var bookkeeping: Bool?
    public var output: String?
    public var diffs: [FileDiff]?
    public var locations: [Location]?
    public var options: [PermissionOption]?
    public var selected: String?
    public var command: String?
    public var detail: String?
    public var cwd: String?
    public var entries: [PlanItem]?
    /// notice: info | error
    public var style: String?
    /// Structured conversation data added to the existing team-tool notice.
    public var botMessage: BotMessage?
    public var heading: String?
    public var delegationId: String?
    public var sourceBotId: String?
    public var targetBotId: String?
    /// The bot that wrote it (a group shows who spoke).
    public var author: String?
    /// user: the message it replies to, as the computer quoted it.
    public var replyTo: ReplyQuote?
    /// The user's emoji reactions, oldest first.
    public var reactions: [String]?
    /// The bots' reactions, one per bot, oldest first.
    public var botReactions: [BotReaction]?
    /// Files sent with a user message.
    public var attachments: [Attachment]?
    /// Immutable files explicitly sent by the bot.
    public var files: [SharedFile]?
    /// notice: a finished voice call's length.
    public var callSeconds: Int?
    /// The voice call it was said in (user), answered during (agent), or that it ends (notice); the chat shows those
    /// messages only in the call's conversation.
    public var call: String?
    /// What the user pasted rather than typed, which memory never takes as their own words: kept for a resend.
    public var pasted: [String]?
    /// user: `voice` when a call's voice model wrote it for the user (`send_to_bot`).
    public var from: String?
    /// notice: what was said on a realtime call, oldest first.
    public var transcript: [CallLine]?
    /// agent: an artifact card, the bot's artifact as published then (the newest version shows).
    public var artifact: ArtifactRef?

    public init(text: String? = nil, status: String? = nil, clientNonce: String? = nil) {
        self.text = text
        self.status = status
        self.clientNonce = clientNonce
    }
}

/// One line of a voice call: what the user said (`from: user`) or the voice answered (`voice`), with when (ms).
public struct CallLine: Codable, Hashable, Sendable {
    public var from: String
    public var text: String
    public var at: Int64?
    /// The app's id for the line: one sent again (its first answer lost) is kept once.
    public var id: String?

    public init(from: String, text: String, at: Int64?, id: String? = nil) {
        self.from = from
        self.text = text
        self.at = at
        self.id = id
    }
}

/// The message a reply quotes: its entry, who wrote it (`user` or a bot id) and its text (at most 500
/// characters, or its file names), and the artifact when it is an artifact card. A partial quote still decodes, so the
/// message itself always shows.
public struct ReplyQuote: Codable, Hashable, Sendable {
    public var id: String
    public var author: String
    public var text: String
    public var artifact: String?

    public init(id: String, author: String, text: String, artifact: String? = nil) {
        self.id = id
        self.author = author
        self.text = text
        self.artifact = artifact
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        author = try c.decodeIfPresent(String.self, forKey: .author) ?? ""
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        artifact = try c.decodeIfPresent(String.self, forKey: .artifact)
    }
}

/// A memory line from an older host, known only to keep it out of the chat.
public struct MemoryLine: Codable, Hashable, Sendable {}

/// One bot's emoji on a message.
public struct BotReaction: Codable, Hashable, Sendable {
    public var bot: String
    public var emoji: String
}

extension EntryData {
    /// The bots' reactions by emoji, in the order each first appeared.
    public var botReactionGroups: [(emoji: String, bots: [String])] {
        (botReactions ?? []).reduce(into: []) { groups, r in
            if let i = groups.firstIndex(where: { $0.emoji == r.emoji }) {
                groups[i].bots.append(r.bot)
            } else {
                groups.append((r.emoji, [r.bot]))
            }
        }
    }
}

/// A file sent with a message; the agent reads it on the computer.
public struct Attachment: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var name: String
    public var size: Int64

    public init(id: String, name: String, size: Int64) {
        self.id = id
        self.name = name
        self.size = size
    }

    public var isImage: Bool { Self.isImage(name) }

    public static func isImage(_ name: String) -> Bool {
        ["png", "jpg", "jpeg", "heic", "gif", "webp", "tiff", "bmp"].contains((name as NSString).pathExtension.lowercased())
    }
}


/// 固定成员身份，偏移始终使用实际请求正文的 UTF-16 半开区间。
public struct Mention: Codable, Hashable, Sendable {
    public var botId: String
    public var start: Int
    public var end: Int

    /// 选择成员时创建绑定，名字只保留在正文中。
    public init(botId: String, start: Int, end: Int) { self.botId = botId; self.start = start; self.end = end }

    /// 编辑触及片段时解除绑定；片段外编辑同步移动偏移。
    public static func edited(from before: String, to after: String, mentions: [Mention], replacement: Range<Int>? = nil) -> [Mention] {
        /// 用完整 Unicode 标量判断片段边界，扩展汉字不能按单个高代理误判。
        func nameContinues(at offset: Int) -> Bool {
            guard offset >= 0, offset < after.utf16.count else { return false }
            let position = after.utf16.index(after.utf16.startIndex, offsetBy: offset)
            guard let scalarIndex = position.samePosition(in: after.unicodeScalars), let scalar = after.unicodeScalars[scalarIndex...].first else { return false }
            return CharacterSet.alphanumerics.contains(scalar) || scalar == "_"
        }
        if let replacement {
            let delta = after.utf16.count - before.utf16.count
            // 原生范围解除重叠绑定，保留外部片段并换算 UTF-16 差值。
            return mentions.compactMap { mention in
                let extendsName = mention.end == replacement.lowerBound && delta > 0 && nameContinues(at: mention.end)
                if mention.end <= replacement.lowerBound, !extendsName { return mention }
                if mention.start >= replacement.upperBound { return Mention(botId: mention.botId, start: mention.start + delta, end: mention.end + delta) }
                return nil
            }
        }
        guard before != after else { return mentions }
        let old = Array(before.utf16), new = Array(after.utf16)
        var start = 0, suffix = 0
        while start < old.count, start < new.count, old[start] == new[start] { start += 1 }
        while suffix < old.count, suffix < new.count, old[old.count - suffix - 1] == new[new.count - suffix - 1] { suffix += 1 }
        // 重复字符可能对应多个编辑位置；任何位置触及绑定都解除，避免静默误投。
        let removed = max(0, old.count - new.count, old.count - start - suffix)
        let earliest = old.count - suffix - removed, latestEnd = start + removed
        let delta = new.count - old.count
        // 无原生范围时保守处理重复字符，不能猜测一个可能误投的位置。
        return mentions.compactMap { mention in
            let extendsName = mention.end == earliest && delta > 0 && nameContinues(at: mention.end)
            if mention.end <= earliest, !extendsName { return mention }
            if mention.start >= latestEnd { return Mention(botId: mention.botId, start: mention.start + delta, end: mention.end + delta) }
            return nil
        }
    }
}

/// 同一来源事件和 attempt 卡片内的一个目标，名字由客户端当前成员列表呈现。
public struct DeliveryRow: Codable, Hashable, Sendable {
    public var deliveryId: String
    public var targetBotId: String
    public var status: String
    public var detail: String

    /// 内部状态统一显示为中文；未知状态不暴露协议字段。
    public var statusText: String {
        switch status {
        case "queued": "等待执行"
        case "claimed": "准备执行"
        case "started": "执行中"
        case "completed": "已完成"
        case "failed": "失败"
        case "interrupted": "已中断"
        case "noReply": "未回复"
        case "cancelled": "已停止"
        default: "状态待更新"
        }
    }

    /// 默认一行只聚合状态；仅全部因同一保护规则停止时，展示该停止原因。
    public static func summary(_ rows: [DeliveryRow]) -> String {
        guard let first = rows.first else { return "群内派工状态待更新" }
        if rows.allSatisfy({ $0.status == "cancelled" && $0.detail == first.detail }) {
            switch first.detail {
            case "对话轮次上限": return "自动接力已达 3 轮，已停止继续派工"
            case "群派工链深上限": return "自动接力已达链深上限，已停止继续派工"
            case "房间发言上限": return "本轮发言已达上限，已停止继续派工"
            default: break
            }
        }
        var counts: [String: Int] = [:]
        var order: [String] = []
        for row in rows {
            if counts[row.statusText] == nil { order.append(row.statusText) }
            counts[row.statusText, default: 0] += 1
        }
        if order.count == 1 { return "群内派工\(first.statusText)" }
        return "群内派工：" + order.map { "\(counts[$0, default: 0]) 项\($0)" }.joined(separator: "，")
    }
}
