import CodyncKit
import SwiftUI

/// 在已知群的成员中选择下一阶段角色，展示并确认可继承的配置。
public struct StageGroupView: View {
    @Environment(BotStore.self) private var store
    @Environment(\.dismissModal) private var dismiss
    let group: Bot
    @State private var requestId = UUID()
    @State private var name = ""
    @State private var goal = ""
    @State private var acceptance = ""
    @State private var leaderId = ""
    @State private var handoffVersion: String?
    @State private var roles: [String: StageGroupDraft.Role] = [:]
    @State private var saving = false
    @State private var error: String?

    /// 将表单绑定到来源群，成员变更由所属电脑同步。
    public init(group: Bot) { self.group = group }

    /// 预览阶段配置，提交成功后由 Store 打开聊天。
    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader("Create next stage group")
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    TextField(L10n.tr("Stage name"), text: $name).fieldBox(fill: Palette.surface)
                    TextField(L10n.tr("Stage goal"), text: $goal, axis: .vertical).fieldBox(fill: Palette.surface)
                    TextField(L10n.tr("Acceptance criteria"), text: $acceptance, axis: .vertical).fieldBox(fill: Palette.surface)
                    if let stage = group.stage {
                        if let handoff = stage.handoff, handoff.acceptedAt != nil {
                            Toggle(isOn: Binding(get: { handoffVersion == handoff.file.id },
                                set: { handoffVersion = $0 ? handoff.file.id : nil })) {
                                Text("\(L10n.tr("Use accepted handoff")) · \(handoff.file.sha256.prefix(12))")
                            }.toggleStyle(.codync)
                        } else {
                            Text(L10n.tr("Review and accept a handoff in Stage details first."))
                                .font(.footnote).foregroundStyle(Palette.secondary)
                        }
                    }
                    Text(L10n.tr("New bots start with their own workspace and Codync shared memory off."))
                        .font(.footnote).foregroundStyle(Palette.secondary)
                    Text(L10n.tr("Keep only stable instructions for the selected roles."))
                        .font(.footnote).foregroundStyle(Palette.secondary)
                    ForEach(group.members.compactMap { store.bots[$0] }) { bot in
                        // 保留来源顺序，勾选时才展示可编辑的角色配置。
                        roleFields(bot)
                    }
                    if let error { Text(L10n.tr(error)).font(.footnote).foregroundStyle(Palette.danger) }
                    Button(L10n.tr(saving ? "Creating…" : "Create stage group")) { save() }
                        .buttonStyle(.primary)
                        .disabled(saving || leaderId.isEmpty || (group.stage != nil && handoffVersion == nil)
                            || [name, goal, acceptance].contains { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty })
                }
                .padding(20)
            }
        }
        .background(Palette.background)
    }

    /// 角色说明可在创建前确认，项目目录仅在用户选择时继承。
    private func roleFields(_ bot: Bot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle(bot.name, isOn: Binding(get: { roles[bot.id] != nil }, set: { on in
                // 取消当前负责人时要求用户重新明确选择。
                if !on && leaderId == bot.id { leaderId = "" }
                roles[bot.id] = on ? StageGroupDraft.Role(botId: bot.id, sourceRev: bot.rev, description: bot.description) : nil
            })).toggleStyle(.codync)
            if roles[bot.id] != nil {
                Text([bot.backend, bot.model ?? ""].filter { !$0.isEmpty }.joined(separator: " · "))
                    .font(.caption).foregroundStyle(Palette.secondary)
                TextField(L10n.tr("Role instructions"), text: Binding(
                    get: { roles[bot.id]?.description ?? "" },
                    set: { roles[bot.id]?.description = $0 }
                ), axis: .vertical).fieldBox(fill: Palette.surface)
                if !bot.cwd.isEmpty && !bot.managedWorkspace {
                    Toggle(L10n.tr("Reuse project folder"), isOn: Binding(
                        get: { roles[bot.id]?.reuseProject ?? false },
                        set: { roles[bot.id]?.reuseProject = $0 }
                    )).toggleStyle(.codync)
                    Text(bot.cwd).font(.caption).foregroundStyle(Palette.secondary)
                }
                if bot.stageMemoryShared == true {
                    Text(L10n.tr("This agent shares its own memory across sessions."))
                        .font(.caption).foregroundStyle(Palette.secondary)
                }
                Button(L10n.tr(leaderId == bot.id ? "Stage lead" : "Set as stage lead")) { leaderId = bot.id }
                    .buttonStyle(.secondary).disabled(leaderId == bot.id)
            }
        }
        .disabled(saving)
    }

    /// 用同一个创建 ID 重试，失败时保留预览字段。
    private func save() {
        let draft = StageGroupDraft(botId: group.id, requestId: requestId, name: name, goal: goal, acceptance: acceptance,
                                    leaderId: leaderId, roles: group.members.compactMap { roles[$0] }, handoffVersion: handoffVersion)
        saving = true
        error = nil
        Task {
            defer { saving = false }
            do {
                try await store.createStageGroup(draft)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// 查看阶段状态并以人工确认保存交接，验收与关闭分别操作。
public struct StageDetailsView: View {
    @Environment(BotStore.self) private var store
    @Environment(\.dismissModal) private var dismiss
    private let botId: String
    @State private var details: StageDetails?
    @State private var markdown = ""
    @State private var evidence = ""
    @State private var baseVersion: String?
    @State private var incomingOpen = false
    @State private var busy = false
    @State private var error: String?

    /// 绑定当前阶段群，不随列表中的其它选择变化。
    public init(botId: String) { self.botId = botId }

    /// 先展示保存状态和待处理项，再提供可用的人工操作。
    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader("Stage details")
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if let details, let stage = details.bot.stage { content(details, stage) }
                    Button(L10n.tr("Refresh")) { Task { await load() } }.buttonStyle(.secondary).disabled(busy)
                    if let error { Text(L10n.tr(error)).font(.footnote).foregroundStyle(Palette.danger) }
                    if details != nil { StageUsageView(botId: botId) }
                }.padding(20)
            }
        }.background(Palette.background).task(id: botId) { await load() }
    }

    /// 编辑只在开放阶段可用，查看交接和重开操作在关闭后仍可用。
    @ViewBuilder private func content(_ details: StageDetails, _ stage: StageGroup) -> some View {
        let handoff = stage.handoff
        let closed = stage.closedAt != nil
        let pending = !details.work.pending.isEmpty
        let changed = markdown.trimmingCharacters(in: .whitespacesAndNewlines) != (details.markdown ?? "")
            || evidence.trimmingCharacters(in: .whitespacesAndNewlines) != (handoff?.evidence ?? "")
        Text("\(details.bot.name) · \(L10n.tr(closed ? "Stage closed" : "Stage open"))").font(.headline)
        Text(L10n.tr(handoff?.acceptedAt == nil ? "Stage awaiting acceptance" : "Stage accepted")).font(.footnote)
        ForEach(details.work.counts.keys.sorted(), id: \.self) { status in
            Text("\(L10n.tr(status)) · \(details.work.counts[status] ?? 0)").font(.footnote)
        }
        ForEach(details.work.pending) { item in Text("\(item.name) · \(L10n.tr(item.status))").font(.footnote) }
        if let incoming = details.incomingMarkdown {
            Button(L10n.tr("Incoming handoff")) { Motion.animate { incomingOpen.toggle() } }.buttonStyle(.secondary)
            if incomingOpen { Text(incoming).textSelection(.enabled) }
        }
        if details.handoffUnavailable || details.incomingUnavailable {
            Text(L10n.tr("The handoff file is unavailable.")).foregroundStyle(Palette.danger)
        }
        if let handoff { Text("\(handoff.file.name) · \(handoff.file.sha256.prefix(12))").font(.caption) }
        if closed {
            if let saved = details.markdown { Text(saved).textSelection(.enabled) }
            if let references = handoff?.evidence, !references.isEmpty { Text(references).textSelection(.enabled) }
        } else {
            TextField(L10n.tr("Handoff Markdown"), text: $markdown, axis: .vertical).fieldBox(fill: Palette.surface).disabled(busy)
            TextField(L10n.tr("Durable evidence references"), text: $evidence, axis: .vertical).fieldBox(fill: Palette.surface).disabled(busy)
            if let files = details.sharedFiles, !files.isEmpty {
                Text(L10n.tr("Shared evidence files")).font(.footnote)
                ForEach(files, id: \.id) { file in
                    let reference = "file:\(file.id)"
                    Button { evidence = [evidence.trimmingCharacters(in: .whitespacesAndNewlines), reference].filter { !$0.isEmpty }.joined(separator: "\n") } label: { Text("\(file.name) · \(file.sha256.prefix(12))") }
                        .buttonStyle(.secondary).disabled(busy || evidence.components(separatedBy: .newlines).contains(reference))
                }
            }
            Button(L10n.tr("Save handoff")) {
                Task { await load(StageChange(botId: botId, action: .saveHandoff, markdown: markdown,
                                               evidence: evidence, previousVersion: baseVersion)) }
            }.buttonStyle(.secondary).disabled(busy || !changed || markdown.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        if let handoff {
            Button(L10n.tr("Accept stage")) { Task { await load(StageChange(botId: botId, action: .accept, version: handoff.file.id)) } }
                .buttonStyle(.secondary).disabled(busy || changed || baseVersion != handoff.file.id || handoff.evidence.isEmpty || handoff.acceptedAt != nil || pending || details.handoffUnavailable)
        }
        Button(L10n.tr(closed ? "Reopen stage" : "Close stage")) {
            Task { await load(StageChange(botId: botId, action: closed ? .reopen : .close)) }
        }.buttonStyle(.secondary).disabled(busy || (!closed && pending))
        if !closed && details.bot.lastMessage == nil && store.composerDraft(for: botId).isEmpty {
            Button(L10n.tr("Restore first draft")) { store.restoreStageDraft(details); dismiss() }.buttonStyle(.secondary)
        }
    }

    /// 只在初次读取或确认保存后替换编辑内容，网络失败保留当前输入。
    private func load(_ change: StageChange? = nil) async {
        busy = true
        error = nil
        defer { busy = false }
        do {
            let state = try await store.stageDetails(botId, change: change)
            if details == nil || change?.action == .saveHandoff {
                markdown = state.markdown ?? state.template
                evidence = state.bot.stage?.handoff?.evidence ?? ""
                baseVersion = state.bot.stage?.handoff?.file.id
            }
            details = state
        } catch { self.error = error.localizedDescription }
    }
}

/// 在详情中按需查看本机观测和每个模型字段的报告比例。
private struct StageUsageView: View {
    @Environment(BotStore.self) private var store
    let botId: String
    @State private var usage: StageUsage?
    @State private var busy = false
    @State private var error: String?

    /// 使用 Host 选定的显示顺序，数字使用设备当前格式。
    var body: some View {
        Button(L10n.tr(usage == nil ? "Stage usage" : "Refresh stage usage")) {
            Task { await load() }
        }.buttonStyle(.secondary).disabled(busy)
        if let error { Text(L10n.tr(error)).font(.footnote).foregroundStyle(Palette.danger) }
        if let usage {
            let since = Date(timeIntervalSince1970: Double(usage.observedSince) / 1000).formatted(date: .abbreviated, time: .omitted)
            Text(L10n.tr("Recording since {date}").replacingOccurrences(of: "{date}", with: since)).font(.footnote)
            if usage.stageCreatedAt < usage.observedSince { Text(L10n.tr("Earlier attempts may be missing.")).font(.footnote) }
            Text(L10n.tr("Reported fields may overlap. Account usage is separate.")).font(.footnote)
            ForEach(usage.sections.indices, id: \.self) { index in
                let section = usage.sections[index]
                let subject = section.backend.map { " · \($0) · \(section.model ?? L10n.tr("Unknown model"))" } ?? ""
                Text(L10n.tr(section.label) + subject).font(.headline)
                ForEach(section.rows.indices, id: \.self) { rowIndex in
                    let row = section.rows[rowIndex]
                    let number = row.value?.formatted() ?? L10n.tr("Unavailable")
                    let coverage = row.reports.map { " (\($0)/\(row.attempts ?? 0))" } ?? ""
                    Text("\(L10n.tr(row.label)) · \(number)\(coverage)").font(.footnote)
                }
            }
        }
    }

    /// 网络失败单独反馈，已显示的用量仍可查看。
    private func load() async {
        busy = true
        error = nil
        defer { busy = false }
        do { usage = try await store.stageUsage(botId) }
        catch { self.error = error.localizedDescription }
    }
}
