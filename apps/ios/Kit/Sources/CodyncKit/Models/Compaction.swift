import Foundation

/// "Earlier context was summarized": the harness's summary, when it gave one. Every field is optional so a newer
/// host's line still decodes.
public struct Compaction: Codable, Hashable, Sendable {
    public var summary: String?
    /// The host started it, ahead of time; otherwise the harness did when its context filled.
    public var byHost: Bool?
}

/// What a new conversation carries when the bot's own chat starts over: editable until the user's next message
/// there sends it.
public struct HandoffCard: Codable, Hashable, Sendable {
    /// draft · approved · sent · kept
    public var status: String?
    /// cold · away · full · capacity · requested
    public var reason: String?
    public var text: String?

    public var isDraft: Bool { status == "draft" }
}

extension HostClient {
    /// Saves the user's version of a handoff that hasn't been sent.
    public func editHandoff(entryId: String, text: String) async throws {
        struct Body: Encodable { var entryId: String; var text: String }
        let _: Empty = try await call("editHandoff", Body(entryId: entryId, text: text))
    }

    /// Carries on with the old conversation instead of starting over.
    public func keepConversation(entryId: String) async throws {
        let _: Empty = try await call("keepConversation", ["entryId": entryId])
    }
}

/// 群成员的上下文总览由 Host 统一提供，各成员共用同一显示基准。
public struct GroupSessionContexts: Decodable, Sendable {
    public var basis: Int
    public var members: [Member]

    public struct Member: Decodable, Sendable, Identifiable {
        public var id: String { botId }
        public var botId: String
        public var hasSession: Bool
        public var used: Int?
        public var size: Int?
        public var actualModelName: String?
        public var reasoning: String?
        public var shared: Bool
        public var recommended: Bool
        public var handoff: Review?
    }

    /// 阻塞派工、私聊跳转和主动摘要共用同一结果类型。
    public struct Review: Decodable, Sendable {
        public var phase: String?
        public var deliveryId: String?
        public var privateEntryId: String?
        public var text: String?
        public var revision: String?
    }
}
