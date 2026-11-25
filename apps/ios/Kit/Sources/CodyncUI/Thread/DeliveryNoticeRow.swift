import CodyncKit
import SwiftUI

/// 暂停后保留任务身份和摘要，用户仍能在原群继续。
struct HeldDeliveryRow: View {
    let chat: Bot
    let task: GroupDeliveryControl
    @Environment(BotStore.self) private var model

    /// 等待重新执行时显示状态，收到新的开始事件后由活动标签接管。
    var body: some View {
        let paused = task.status == "paused"
        let status = L10n.tr(paused ? "Paused" : "Continuing…")
        let name = model.bots[task.targetBotId]?.name ?? L10n.tr("Bot")
        HStack(spacing: 8) {
            Text("\(name) · \(status)" + (task.preview.isEmpty ? "" : " · \(task.preview)"))
                .font(.subheadline).lineLimit(1)
            if paused {
                IconButton("Continue", systemImage: "play.fill") {
                    // 继续同一个派工，Host 负责安排空闲时执行。
                    model.setDeliveryPaused(chat.id, task: task, paused: false)
                }
                .disabled(chat.stage?.closedAt != nil || chat.stage?.archivedAt != nil)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, paused ? 4 : 12)
        .foregroundStyle(Palette.secondary)
        .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
    }
}

extension EnvironmentValues {
    @Entry var openedDispatchId: String?
}

/// 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。
struct DeliveryNoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.openedDispatchId) private var openedDispatchId
    @State private var expanded: Bool
    private let deliveryId: String?

    /// 成员弹页聚焦一条原派工，聊天中的卡片保持默认折叠。
    init(entry: Entry, deliveryId: String? = nil) {
        self.entry = entry
        self.deliveryId = deliveryId
        _expanded = State(initialValue: deliveryId != nil)
    }

    /// 仅显示同群中可接收引导的回合，提交时仍由 host 复核。
    private func activeId(_ row: DeliveryRow) -> String? {
        guard row.canGuide == true, row.status == "queued", model.connection == .online,
              let bot = model.bots[row.targetBotId], bot.status == "working", bot.storageError == nil,
              bot.workingChat == entry.botId else { return nil }
        return bot.guidingDeliveryId
    }

    /// 折叠状态属于本地展示，投递状态和操作结果来自 host。
    var body: some View {
        let rows = (entry.data.deliveryRows ?? []).filter { deliveryId == nil || $0.deliveryId == deliveryId }
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
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 8) {
                        Text("\(model.bots[row.targetBotId]?.name ?? "已移除成员")：\(row.statusText)" + (row.detail.isEmpty ? "" : " · \(row.detail)"))
                            .font(.footnote).foregroundStyle(Palette.secondary)
                        if let id = activeId(row) {
                            Button("引导") { model.guideDelivery(row.deliveryId, activeId: id) }.buttonStyle(.secondary)
                        } else if ["failed", "interrupted", "noReply"].contains(row.status) {
                            Button("重新尝试") { model.retryDelivery(row.deliveryId) }.buttonStyle(.secondary)
                        }
                    }
                        DeliveryActivityView(row: row, roomId: entry.botId)
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
        .onChange(of: openedDispatchId, initial: true) { _, id in
            // 来自成员活动的定位应展开原派工。
            if id == entry.id { expanded = true }
        }
        if model.hello?.groupSessionContexts != true {
            ForEach(rows, id: \.deliveryId) { row in
                if let handoff = row.groupHandoff { GroupDeliveryHandoffRow(row: row, handoff: handoff) }
            }
        }

    }
}

/// 时间线使用本地时间；完成的结果从来源群读取。
private struct DeliveryActivityView: View {
    let row: DeliveryRow
    let roomId: String
    @Environment(BotStore.self) private var model
    @Environment(\.jumpToChatEntry) private var jumpToChatEntry
    @State private var result: Result?
    @State private var loading = false
    @State private var error: String?

    private struct Result: Decodable, Sendable {
        let entry: Entry
        let evidence: Entry?
    }

    /// 终态下保留工具开始记录，只有执行中才标记正在运行。
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let startedAt = row.startedAt {
                Text("\(time(startedAt)) · \(L10n.tr("Started assignment"))")
            }
            ForEach(row.activity ?? []) { step in
                let status = step.status == "completed" ? "Step completed" : step.status == "failed" ? "Step failed" : step.status == "interrupted" ? "Step interrupted"
                    : row.status == "started" ? "In progress" : "Step started"
                Text("\(time(step.at)) · \(L10n.tr(step.label)) · \(L10n.tr(status))")
                    .foregroundStyle(status == "In progress" ? Palette.text : Palette.secondary)
            }
            if let id = row.resultId {
                Button(L10n.tr("View result")) {
                    // 已加载的结果直接定位；其他结果从 Host 读取。
                    if let jumpToChatEntry, model.entries[roomId]?.contains(where: { $0.id == id }) == true {
                        jumpToChatEntry(id)
                    } else {
                        Task { await openResult(id) }
                    }
                }.buttonStyle(.secondary).disabled(loading)
            }
            if let error { Text(error).foregroundStyle(Palette.danger) }
        }
        .font(.footnote).foregroundStyle(Palette.secondary)
        .codyncSheet(isPresented: Binding(get: { result != nil }, set: { if !$0 { result = nil } })) {
            VStack(spacing: 0) {
                ModalHeader("View result")
                CardForm {
                    Text(result?.entry.data.text ?? "")
                    if let evidence = result?.evidence {
                        ForEach(evidence.data.files ?? []) { file in
                            // 证据下载沿用原群的附件消息权限。
                            FileDownloadCard(entry: evidence, file: file) { [] }
                        }
                    }
                }
            }
        }
    }

    /// 用设备设置的时区格式化群内事件的时间。
    private func time(_ at: Int64) -> String {
        Date(timeIntervalSince1970: Double(at) / 1000).formatted(date: .omitted, time: .shortened)
    }

    /// 预览的消息身份由 Host 按群和投递结果重新检查。
    private func openResult(_ id: String) async {
        loading = true
        error = nil
        defer { loading = false }
        do {
            let reply: Result = try await model.ready().call("groupDeliveryResult", ["roomId": roomId, "resultId": id])
            result = reply
        } catch {
            self.error = error.localizedDescription
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
