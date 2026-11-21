import CodyncKit
import SwiftUI

/// 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。
struct DeliveryNoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false

    /// 仅显示同群中可接收引导的回合，提交时仍由 host 复核。
    private func activeId(_ row: DeliveryRow) -> String? {
        guard row.canGuide == true, row.status == "queued", model.connection == .online,
              let bot = model.bots[row.targetBotId], bot.status == "working", bot.storageError == nil,
              bot.workingChat == entry.botId else { return nil }
        return bot.guidingDeliveryId
    }

    /// 折叠状态属于本地展示，投递状态和操作结果来自 host。
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
        // 绑定渲染时的回合身份，点击迟到时由 host 拒绝旧操作。
        let eligible = rows.compactMap { row in activeId(row).map { (row.deliveryId, $0) } }
        HStack(alignment: .top, spacing: 8) {
            Disclosure(isExpanded: $expanded) {
                Text("第 \(entry.data.deliveryAttempt ?? 1) 次投递" + (entry.data.parentDeliveryId != nil ? " · 重试投递" : ""))
                    .font(.footnote).foregroundStyle(Palette.secondary)
                ForEach(rows, id: \.deliveryId) { row in
                    // 明确选择一个成员；其他成员的投递状态由 host 独立更新。
                    HStack(spacing: 8) {
                        Text("\(model.bots[row.targetBotId]?.name ?? "已移除成员")：\(row.statusText)" + (row.detail.isEmpty ? "" : " · \(row.detail)"))
                            .font(.footnote).foregroundStyle(Palette.secondary)
                        if let id = activeId(row) {
                            Button("引导") { model.guideDelivery(row.deliveryId, activeId: id) }.buttonStyle(.secondary)
                        } else if ["failed", "interrupted", "noReply"].contains(row.status) {
                            Button("重新尝试") { model.retryDelivery(row.deliveryId) }.buttonStyle(.secondary)
                        }
                    }
                }
            } label: {
                Text(summary).font(.footnote).foregroundStyle(Palette.secondary).lineLimit(1)
            }
            if !expanded, let (deliveryId, activeId) = eligible.first {
                Button(eligible.count == 1 ? "引导" : "引导…") {
                    // 多位成员可以接收引导时，先展开列表明确选择目标。
                    if eligible.count == 1 { model.guideDelivery(deliveryId, activeId: activeId) }
                    else { withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { expanded = true } }
                }.buttonStyle(.secondary)
            }
        }
        if model.hello?.groupSessionContexts != true {
            ForEach(rows, id: \.deliveryId) { row in
                if let handoff = row.groupHandoff { GroupDeliveryHandoffRow(row: row, handoff: handoff) }
            }
        }

    }
}

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
