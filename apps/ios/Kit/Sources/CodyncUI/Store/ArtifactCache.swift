import CodyncKit
import Foundation

/// One computer's artifacts: their details by id (from `library`, `readArtifact` and the `artifact` events), the
/// versions their cards announced, and the HTML of the ones shown with the version it is, read again once a newer one
/// is known.
struct ArtifactCache {
    struct HTML: Equatable {
        let updatedAt: Int64
        let text: String
    }

    private(set) var details: [String: Artifact] = [:]
    private(set) var html: [String: HTML] = [:]
    /// 释放这个已删除聊天的作品内容和版本状态。
    mutating func removeBot(_ botId: String) {
        deletedBots.insert(botId)
        let ids = details.values.filter { $0.botId == botId }.map(\.id)
        for id in ids {
            details[id] = nil
            html[id] = nil
            announced[id] = nil
            failures[id] = nil
            reading.remove(id)
        }
    }

    /// The newest version each artifact's cards announced (an entry can bring one the details missed while away).
    private var announced: [String: Int64] = [:]
    private var deletedBots: Set<String> = []
    var failures: [String: String] = [:]
    var reading: Set<String> = []

    /// 删除后到达的事件或旧版详情不会覆盖当前缓存。
    mutating func upsert(_ artifact: Artifact) {
        guard !deletedBots.contains(artifact.botId) else { return }
        if let known = details[artifact.id], known.updatedAt > artifact.updatedAt { return }
        details[artifact.id] = artifact
    }

    mutating func announce(_ card: ArtifactRef) {
        announced[card.id] = max(announced[card.id] ?? 0, card.updatedAt)
    }

    /// 只有聊天仍存活时才保留加载完成的 HTML。
    mutating func store(_ artifact: Artifact, html text: String) {
        guard !deletedBots.contains(artifact.botId) else { return }
        html[artifact.id] = HTML(updatedAt: artifact.updatedAt, text: text)
        upsert(artifact)
    }

    /// The newest version of an artifact known here.
    func newest(_ id: String) -> Int64 {
        max(announced[id] ?? 0, details[id]?.updatedAt ?? 0)
    }

    /// The HTML held is older than the newest version known (or missing), and no read is under way.
    func needsRead(_ id: String) -> Bool {
        !reading.contains(id) && (html[id]?.updatedAt ?? -1) < newest(id)
    }

    /// A card's title: the artifact's own once it is as new as the card.
    func title(_ card: ArtifactRef) -> String {
        details[card.id].flatMap { $0.updatedAt >= card.updatedAt ? $0.title : nil } ?? card.title
    }

    func artifacts(of botId: String) -> [Artifact] {
        details.values.filter { $0.botId == botId }.sorted { $0.updatedAt > $1.updatedAt }
    }
}
