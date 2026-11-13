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
    @State private var leaderId: String?
    @State private var showingRoles = false
    @State private var editingRole: String?
    @State private var handoffVersion: String?
    @State private var roles: [String: StageGroupDraft.Role] = [:]
    @State private var saving = false
    @State private var error: String?

    /// 将表单绑定到来源群，成员变更由所属电脑同步。
    public init(group: Bot) { self.group = group }

    private var candidates: [Bot] { group.members.compactMap { store.bots[$0] } }
    private var picked: [Bot] { candidates.filter { roles[$0.id] != nil } }
    private var optional: Bool { store.supportsGroupMemberPicker }

    /// 阶段群沿用统一的成员布局，交接和角色配置在成员区后面。
    public var body: some View {
        let membersOnly = store.supportsOptionalStageSetup
        let validFields = !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && (optional || [goal, acceptance].allSatisfy { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty })
        CreateForm(title: "Create next stage group", placeholder: "Stage name", name: $name, saving: saving,
                   canCreate: !saving && (membersOnly ? !picked.isEmpty : validFields && picked.contains { $0.id == leaderId }
                       && (group.stage == nil || handoffVersion != nil)),
                   error: error, create: save, group: true) {
            GroupAvatar(members: picked, size: 80)
        } content: {
            VStack(alignment: .leading, spacing: 12) {
                GroupMemberPicker(bots: candidates, picked: picked, toggle: toggleRole)
                GroupBotPicker(bots: picked, selection: $leaderId, label: membersOnly ? "Group lead (optional)" : "Group lead", allowsNone: membersOnly)
                TextField(L10n.tr(optional ? "Stage goal (optional)" : "Stage goal"), text: $goal, axis: .vertical)
                    .lineLimit(2...5).fieldBox(fill: Palette.surface)
                TextField(L10n.tr(optional ? "Acceptance criteria (optional)" : "Acceptance criteria"), text: $acceptance, axis: .vertical)
                    .lineLimit(2...5).fieldBox(fill: Palette.surface)
                if let stage = group.stage {
                    if let handoff = stage.handoff, handoff.acceptedAt != nil {
                        Toggle(isOn: Binding(get: { handoffVersion == handoff.file.id },
                            set: { handoffVersion = $0 ? handoff.file.id : nil })) {
                            Text("\(L10n.tr("Use accepted handoff")) · \(handoff.file.sha256.prefix(12))")
                        }.toggleStyle(.codync)
                    } else if !membersOnly {
                        Text(L10n.tr("Review and accept a handoff in Stage details first."))
                            .font(.footnote).foregroundStyle(Palette.secondary)
                    }
                }
                Button {
                    // 设置页始终从第一个有效角色打开。
                    editingRole = picked.first?.id
                    showingRoles = true
                } label: {
                    HStack {
                        Text(L10n.tr("Role settings"))
                        Spacer()
                        Image(systemName: "chevron.right")
                    }.fieldBox(fill: Palette.surface)
                }.buttonStyle(.plain).disabled(picked.isEmpty)
            }
        }
        .codyncSheet(isPresented: $showingRoles) {
            VStack(spacing: 0) {
                ModalHeader("Role settings")
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        GroupBotPicker(bots: picked, selection: $editingRole, label: "Role settings", showsLabel: false)
                        if let bot = picked.first(where: { $0.id == editingRole }) {
                            roleFields(bot)
                        }
                    }.padding(20)
                }
            }.background(Palette.background)
        }
    }

    /// 只复制当前选中的来源角色，移除角色时清除其负责人身份。
    private func toggleRole(_ bot: Bot) {
        withAnimation(Motion.layout) {
            if roles[bot.id] != nil {
                roles[bot.id] = nil
                if leaderId == bot.id { leaderId = nil }
            } else {
                roles[bot.id] = StageGroupDraft.Role(botId: bot.id, sourceRev: bot.rev, description: bot.description)
            }
        }
    }

    /// 编辑角色职责和目录继承，源机器人资料保持独立。
    private func roleFields(_ bot: Bot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text([bot.backend, bot.model ?? ""].filter { !$0.isEmpty }.joined(separator: " · "))
                .font(.caption).foregroundStyle(Palette.secondary)
            Text(L10n.tr("Keep only stable instructions for the selected roles."))
                .font(.footnote).foregroundStyle(Palette.secondary)
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
            Text(L10n.tr("New bots start with their own workspace and Codync shared memory off."))
                .font(.footnote).foregroundStyle(Palette.secondary)
            if bot.stageMemoryShared == true {
                Text(L10n.tr("This agent shares its own memory across sessions."))
                    .font(.caption).foregroundStyle(Palette.secondary)
            }
        }
    }

    /// 用同一个创建 ID 重试，失败时保留预览字段。
    private func save() {
        guard !picked.isEmpty, !saving else { return }
        let lead = picked.first { $0.id == leaderId }?.id ?? ""
        let draft = StageGroupDraft(botId: group.id, requestId: requestId, name: name, goal: goal, acceptance: acceptance,
                                    leaderId: lead, roles: group.members.compactMap { roles[$0] }, handoffVersion: handoffVersion)
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
        if !details.initialDraft.isEmpty && !closed && details.bot.lastMessage == nil && store.composerDraft(for: botId).isEmpty {
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
