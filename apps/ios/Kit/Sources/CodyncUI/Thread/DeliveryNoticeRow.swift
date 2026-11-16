import CodyncKit
import SwiftUI

/// 默认一行中文状态，展开仅显示成员状态和重试，不重复来源正文。
struct DeliveryNoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model
    @State private var expanded = false

    /// 折叠状态属于本地展示，状态更新和重试仍由 host 管理。
    var body: some View {
        let rows = entry.data.deliveryRows ?? []
        let phase = model.hello?.groupSessionContexts == true
            ? ["draft", "approved", "switched", "sent"].first { phase in rows.contains { $0.groupHandoff?.phase == phase } }
            : nil
        let summary = switch phase {
        case "draft": L10n.tr("Waiting for a new session")
        case "sent": L10n.tr("New session started. Original dispatch resumed.")
        case "approved", "switched": L10n.tr("Preparing a new session")
        default: DeliveryRow.summary(rows)
        }
        VStack(alignment: .leading, spacing: 8) {
            Disclosure(isExpanded: $expanded) {
                Text("第 \(entry.data.deliveryAttempt ?? 1) 次投递" + (entry.data.parentDeliveryId != nil ? " · 重试投递" : ""))
                    .font(.footnote).foregroundStyle(Palette.secondary)
                ForEach(entry.data.deliveryRows ?? [], id: \.deliveryId) { row in
                    // 显式重试交给 host 创建新投递，不覆盖旧记录。
                    HStack(spacing: 8) {
                        Text("\(model.bots[row.targetBotId]?.name ?? "已移除成员")：\(row.statusText)" + (row.detail.isEmpty ? "" : " · \(row.detail)"))
                            .font(.footnote).foregroundStyle(Palette.secondary)
                        if ["failed", "interrupted", "noReply"].contains(row.status) {
                            Button("重新尝试") { model.retryDelivery(row.deliveryId) }.buttonStyle(.secondary)
                        }
                    }
                }
            } label: {
                Text(summary)
                    .font(.footnote).foregroundStyle(Palette.secondary).lineLimit(1)
            }
            if model.hello?.groupSessionContexts != true {
                ForEach(rows, id: \.deliveryId) { row in
                    if let handoff = row.groupHandoff { GroupDeliveryHandoffRow(row: row, handoff: handoff) }
                }
            }
        }
    }
}

/// 交接操作与原派工一起显示，普通群引导到机器人私聊。
private struct GroupDeliveryHandoffRow: View {
    let row: DeliveryRow
    let handoff: DeliveryRow.GroupHandoff
    @Environment(BotStore.self) private var model
    @State private var revision = ""
    @State private var draft = ""
    @State private var showing = false
    @State private var busy = false
    @State private var error: String?

    /// 摘要编辑面板关闭后保留 Host 上原来的排队任务。
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(model.bots[row.targetBotId]?.name ?? "已移除成员").font(.footnote.weight(.semibold))
                Text(L10n.tr(handoff.phase == "draft" ? "Session switch needs confirmation" : handoff.phase == "sent" ? "New session started. Original dispatch resumed." : "Preparing a new session"))
                    .font(.footnote).foregroundStyle(handoff.phase == "draft" ? Palette.warning : Palette.secondary)
            }
            if handoff.phase == "draft" {
                Text(L10n.tr("The task still won't fit after compaction. It will resume after you confirm.")).font(.footnote)
                if handoff.shared { Text(L10n.tr("This also switches the bot's private session.")).font(.footnote) }
                Button(L10n.tr(handoff.shared ? "Review handoff in private chat" : "Review handoff and switch session")) { Task { await review() } }
                    .buttonStyle(.secondary).disabled(busy || model.isOffline)
            }
            if let error, !showing { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
        }
        .padding(10).background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 12))
        .codyncSheet(isPresented: $showing) {
            VStack(spacing: 0) {
                ModalHeader("Review handoff and switch session") { EmptyView() }
                CardForm {
                    CardSection("Summary for the new session", footer: "This switches the bot's session in this group. Group history and pending tasks are kept.") {
                        TextField(L10n.tr("Summary for the new session"), text: $draft, axis: .vertical).lineLimit(7...15)
                    }
                    if let error { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
                    Button(L10n.tr("Confirm new session")) { Task { await confirm() } }
                        .buttonStyle(.primary).disabled(busy || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    Button(L10n.tr("Not now")) { showing = false }.buttonStyle(.secondary).disabled(busy)
                }
            }
        }
    }

    /// 通过现有聊天选择进入私聊，独立群通过 API 读取摘要。
    private func review() async {
        if handoff.shared { model.selection = row.targetBotId; return }
        busy = true
        defer { busy = false }
        do {
            let result = try await model.groupHandoff(row.deliveryId)
            revision = result.revision
            draft = result.text
            error = nil
            showing = true
        } catch { self.error = error.localizedDescription }
    }

    /// 保存当前输入并确认原派工，失败时保留编辑内容。
    private func confirm() async {
        busy = true
        defer { busy = false }
        do {
            try await model.confirmGroupHandoff(row.deliveryId, revision: revision, text: draft)
            showing = false
        } catch { self.error = error.localizedDescription }
    }
}

@MainActor extension BotStore {
    /// 读取独立群原派工的摘要，私聊会话的摘要仅由其私聊卡提供。
    func groupHandoff(_ id: String) async throws -> (text: String, revision: String) {
        struct Draft: Decodable { var text: String; var revision: String }
        let draft: Draft = try await ready().call("groupHandoff", ["deliveryId": id])
        return (draft.text, draft.revision)
    }

    /// 将编辑和确认一起交给 Host，由原派工继续执行。
    func confirmGroupHandoff(_ id: String, revision: String, text: String) async throws {
        let _: Empty = try await ready().call("confirmGroupHandoff", ["deliveryId": id, "revision": revision, "text": text])
    }
}
