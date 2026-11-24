import CodyncKit
import SwiftUI

/// 私聊和独立群成员共用历史页，旧 Agent 会话的删除需独立确认。
struct SessionHistoryView: View {
    let botId: String
    var roomId: String?
    @Environment(BotStore.self) private var model
    @Environment(\.jumpToChatEntry) private var jumpToChatEntry
    @State private var showing = false
    @State private var snapshot: Snapshot?
    @State private var messages: [Message]?
    @State private var pending: OldSession?
    @State private var busy = false
    @State private var error: String?

    private struct OldSession: Decodable, Sendable, Identifiable {
        let id: String
        let at: Int64
        let state: String
        let hasMessages: Bool
        let canDelete: Bool
    }

    private struct Snapshot: Decodable, Sendable {
        let active: Bool
        let sessions: [OldSession]
    }

    private struct Message: Decodable, Sendable, Identifiable {
        let id: String
        let kind: String
        let author: String?
        let text: String
        let at: Int64
    }

    private struct Messages: Decodable, Sendable { let messages: [Message] }

    /// 关页清除当前预览，Host 继续保留原历史。
    var body: some View {
        Button(L10n.tr("Session history")) {
            // 重新打开时从 Host 读取最新会话状态。
            messages = nil
            snapshot = nil
            error = nil
            showing = true
        }
        .buttonStyle(.secondary).disabled(model.isOffline)
        .codyncSheet(isPresented: $showing) {
            VStack(spacing: 0) {
                ModalHeader(messages == nil ? "Session history" : "Chat history")
                CardForm {
                    if let error { Text(error).foregroundStyle(Palette.danger) }
                    if let messages {
                        Button(L10n.tr("Back")) { self.messages = nil }.buttonStyle(.secondary)
                        CardSection(nil) {
                            if messages.isEmpty { Text(L10n.tr("No messages")) }
                            ForEach(messages) { message in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("\(model.bots[message.author ?? ""]?.name ?? L10n.tr(message.kind == "user" ? "You" : "Bot")) · \(date(message.at))")
                                        .font(.footnote).foregroundStyle(Palette.secondary)
                                    Text(message.text)
                                    if let jumpToChatEntry, model.entries[roomId ?? botId]?.contains(where: { $0.id == message.id }) == true {
                                        Button(L10n.tr("Open original message")) {
                                            // 先收起历史，聊天负责定位到原消息。
                                            showing = false
                                            jumpToChatEntry(message.id)
                                        }.buttonStyle(.secondary)
                                    }
                                }
                            }
                        }
                    } else {
                        sessions
                    }
                }
            }
            .task { await load() }
            .codyncDialog("Delete old agent session", isPresented: Binding(
                get: { pending != nil }, set: { if !$0 { pending = nil } }
            ), message: "The original chat history will be kept.") {
                // 捕获本次确认对应的历史条目，取消只清空选择。
                guard let item = pending else { return [] }
                return [DialogAction("Delete", destructive: true) { Task { await delete(item.id) } }]
            }
        }
    }

    /// 列表区仅在 Host 确认能力和归属后显示删除入口。
    private var sessions: some View {
        CardSection(nil) {
            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.tr(roomId == nil ? "Current main session" : "Current session"))
                Text(L10n.tr(snapshot?.active == true ? "In use" : "No session yet")).foregroundStyle(Palette.secondary)
            }
            if let snapshot {
                if snapshot.sessions.isEmpty { Text(L10n.tr("No old sessions")) }
                ForEach(snapshot.sessions) { item in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(date(item.at))
                        Text(L10n.tr(status(item.state))).font(.footnote).foregroundStyle(Palette.secondary)
                        if item.hasMessages {
                            Button(L10n.tr("View chat history")) { Task { await read(item.id) } }.buttonStyle(.secondary)
                        }
                        if item.canDelete {
                            Button(L10n.tr("Delete old agent session")) { pending = item }.buttonStyle(.secondary)
                        }
                    }.disabled(busy || model.isOffline)
                }
            } else if error == nil { Text(L10n.tr("Loading…")) }
        }
    }

    /// 为 Host 构造同一私聊或独立群作用域。
    private func arguments(_ id: String? = nil) -> [String: String] {
        var arguments = ["botId": botId]
        arguments["roomId"] = roomId
        arguments["historyId"] = id
        return arguments
    }

    /// 根据 Host 已确认的状态选取文案。
    private func status(_ state: String) -> String {
        switch state {
        case "archived": "Archived"
        case "retained": "Retained"
        case "deleted": "Only chat history remains"
        default: "Archive status unknown"
        }
    }

    /// 切换时间沿用设备时区。
    private func date(_ at: Int64) -> String {
        Date(timeIntervalSince1970: Double(at) / 1000).formatted(date: .abbreviated, time: .shortened)
    }

    /// 加载列表，失败时保留已有列表。
    private func load() async {
        do {
            snapshot = try await model.ready().call("sessionHistory", arguments())
        } catch {
            if !Task.isCancelled { self.error = error.localizedDescription }
        }
    }

    /// 读取旧会话对应的原聊天片段。
    private func read(_ id: String) async {
        busy = true
        error = nil
        defer { busy = false }
        do {
            let result: Messages = try await model.ready().call("sessionHistoryMessages", arguments(id))
            messages = result.messages
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// 删除成功后刷新旧条目，失败时保留查看聊天入口。
    private func delete(_ id: String) async {
        busy = true
        error = nil
        defer { busy = false }
        do {
            let _: [String: String] = try await model.ready().call("deleteOldSession", arguments(id))
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
