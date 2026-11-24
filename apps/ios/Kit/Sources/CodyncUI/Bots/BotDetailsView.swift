import CodyncKit
import SwiftUI

/// The details sheet's pages, as on the desktop inspector.
enum DetailsTab: Hashable {
    case details, library, memory, settings
}

/// What to open the details sheet on.
struct DetailsRequest: Identifiable {
    let id = UUID()
    var tab: DetailsTab = .details
    /// A routine to open in its editor (Details page).
    var routineId: String?
    /// Another bot's details than the chat's: a group member's, from its notice.
    var botId: String?
}

/// A bot's details (Grok Bot's): who it is, then its details, library, memory and settings. Settings save
/// as you edit.
struct BotDetailsView: View {
    let botId: String
    let routineId: String?
    /// An artifact picked in the Library, for the chat's viewer.
    let openArtifact: (ArtifactTarget) -> Void
    @State var tab: DetailsTab
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    @State private var draft: BotDraft?
    @State private var saveError: String?
    /// Off once the page closes: a save that fails after that shows over the app instead.
    @State private var presence = PagePresence()
    @State private var saving: Task<Void, Never>?
    @State private var template: EditorRequest?
    private let offersSessionHandoff: Bool

    init(botId: String, request: DetailsRequest, openArtifact: @escaping (ArtifactTarget) -> Void) {
        self.botId = botId
        self.openArtifact = openArtifact
        routineId = request.routineId
        offersSessionHandoff = request.botId == nil
        _tab = State(initialValue: request.tab)
    }

    private var bot: Bot? { model.bots[botId] }

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                if let bot { identity(bot) }
                TabChoice(selection: $tab, options: [(.details, "Details")] + (model.hasLibrary ? [(.library, "Library")] : [])
                    + [(.memory, "Memory"), (.settings, "Settings")])
                    .padding(.top, 18)
                page.padding(.top, 20)
                    .environment(\.cardStyle, .outlined)
                    .environment(\.noteLines, 2)
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Palette.background)
        .overlay(alignment: .topTrailing) { actions }
        .tracked(.screenViewed(.botDetails))
        .task(id: botId) { draft = bot.map(BotDraft.init) }
        // A change made elsewhere (a bot that manages the team, another device) replaces each field not edited here.
        .onChange(of: bot.map(BotDraft.init)) { base, fresh in
            if let base, let fresh, base.id == fresh.id { draft = draft?.rebased(from: base, onto: fresh) }
        }
        .onChange(of: draft) { _, draft in autosave(draft) }
        .onAppear { presence.shown = true }
        .onDisappear { presence.shown = false }
        .codyncSheet(item: $template) { BotTemplateView(draft: $0.draft) }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            if let bot {
                cornerButton("Create template", "square.and.arrow.up") { template = EditorRequest(BotDraft(bot)) }
            }
            cornerButton("Close details", "xmark") { dismiss() }
        }
        .padding(.top, 14)
        .padding(.trailing, 14)
    }

    /// Grok Bot's corner buttons: the page color, ringed by a soft shadow.
    private func cornerButton(_ title: String, _ symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(Palette.text)
                .frame(width: 36, height: 36)
                .background(Palette.background, in: Circle())
                .shadow(color: .black.opacity(0.12), radius: 0.5)
                .shadow(color: .black.opacity(0.04), radius: 2, y: 1)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .help(title)
    }

    /// The bot's avatar and name, and what runs it.
    private func identity(_ bot: Bot) -> some View {
        VStack(spacing: 4) {
            LeadAgentAvatar(bot: bot)
                .padding(.bottom, 10)
            Text(bot.name)
                .font(.title3.weight(.semibold))
                .foregroundStyle(Palette.text)
                .multilineTextAlignment(.center)
            HStack(spacing: 5) {
                AgentIcon(registry: model.agentRegistry(bot.backend), size: 13)
                Text(bot.model.map { "\(model.backendName(bot.backend)) · \($0)" } ?? model.backendName(bot.backend))
            }
            .font(.footnote)
            .foregroundStyle(Palette.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 64)
    }

    @ViewBuilder private var page: some View {
        switch tab {
        case .details:
            VStack(alignment: .leading, spacing: 28) {
                if let bot, model.hello?.botWorkspaceUsage == true { BotWorkspaceUsageView(botId: bot.id) }
                if let bot, !bot.devServers.isEmpty { DevServersSection(bot: bot) }
                if let draft = Binding($draft) {
                    BotSettingsForm(draft: draft, error: saveError, scrolls: false, part: .often)
                }
                if let screen = model.screen, let bot {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(L10n.tr("Computer"))
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(Palette.text)
                            .padding(.leading, 2)
                        SwitchRow("Use the computer",
                                  detail: screen.controlReady == true ? nil : "On that computer, open Codync → Settings → Computer access to set it up first.",
                                  isOn: Binding(get: { bot.computer }, set: { model.setComputer(bot, $0) }))
                            .disabled(model.isOffline)
                            .outlinedCard()
                    }
                }
                if let bot, model.hello?.botSessionUsage == true {
                    SessionUsageView(bot: bot, offersSessionHandoff: offersSessionHandoff).id(bot.id)
                }
                RoutinesView(botId: botId, initialId: routineId)
            }
        case .library:
            LibraryView(botId: botId, openArtifact: openArtifact)
        case .memory:
            MemoryCard(botId: botId)
        case .settings:
            if let draft = Binding($draft) {
                BotSettingsForm(draft: draft, error: saveError, scrolls: false, part: .rarely)
            }
        }
    }

    /// Saves a moment after the last change, like the desktop inspector.
    private func autosave(_ draft: BotDraft?) {
        saving?.cancel()
        guard let draft, let bot, draft != BotDraft(bot), draft.isValid else { return }
        saving = Task {
            try? await Task.sleep(for: .milliseconds(600))
            guard !Task.isCancelled else { return }
            do {
                _ = try await model.save(draft.normalized)
                saveError = nil
            } catch {
                if presence.shown { saveError = error.localizedDescription } else { model.lastError = error.localizedDescription }
            }
        }
    }
}

/// Whether a page is on screen, for work that outlives it.
@MainActor private final class PagePresence {
    var shown = true
}


/// 显示远程 Host 上的 Bot 托管工作区大小。
private struct BotWorkspaceUsageView: View {
    let botId: String
    @Environment(BotStore.self) private var model
    @State private var reading = "Loading…"

    private struct Usage: Decodable, Sendable { let bytes: Int64? }

    /// 重连时重新读取，确认的空目录按 0 B 展示。
    var body: some View {
        CardSection("Disk usage") {
            HStack {
                Text(L10n.tr("Managed workspace"))
                Spacer()
                Text(L10n.tr(model.isOffline ? "Unavailable" : reading))
                    .foregroundStyle(Palette.secondary)
            }
        }
        .task(id: model.isOffline) {
            guard !model.isOffline else { return }
            do {
                let result: Usage = try await model.ready().call("botWorkspaceUsage", ["botId": botId])
                guard !Task.isCancelled else { return }
                reading = result.bytes.map { $0 == 0 ? "0 B" : ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) } ?? "Unavailable"
            } catch {
                if !Task.isCancelled { reading = "Unavailable" }
            }
        }
    }
}

/// 显示主会话占用和最近尝试，展开一次尝试可查看供应方实报字段。
private struct SessionUsageView: View {
    let bot: Bot
    let offersSessionHandoff: Bool
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    @State private var usage: BotSessionUsage?
    @State private var error: String?
    @State private var refresh = 0
    @State private var preparing = false
    @State private var expanded: String?

    /// 将网络动作绑定到详情页生命周期，未知占用保留为不可用。
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            CardSection("Main session context") {
                Text(contextLine)
            } accessory: {
                IconButton("Refresh usage", systemImage: "arrow.clockwise") { refresh += 1 }
                    .disabled(model.isOffline)
            }
            if offersSessionHandoff, model.hello?.sessionHistory == true { SessionHistoryView(botId: bot.id) }
            if let error { Text(L10n.tr(error)).font(.footnote).foregroundStyle(Palette.danger) }
            if offersSessionHandoff, usage?.hasSession == true {
                Button(L10n.tr("Prepare new session with summary")) {
                    // 成功后返回聊天，查看和编辑将带入新会话的摘要。
                    Task { await prepare() }
                }
                .buttonStyle(.secondary)
                .disabled(model.isOffline || bot.isWorking || preparing)
            }
            if let usage {
                CardSection("Recent task attempts") {
                    VStack(alignment: .leading, spacing: 8) {
                        if usage.attempts.isEmpty { Text(L10n.tr("No recent task attempts")) }
                        ForEach(usage.attempts) { attempt in
                            attemptRow(attempt)
                        }
                    }
                }
            }
        }
        .task(id: refreshKey) { await load() }
    }

    /// 只在主会话提交、任务状态和手动刷新的值变化时重新读取统计。
    private var refreshKey: String {
        "\(bot.mainSessionRevision ?? 0)-\(bot.status)-\(model.isOffline)-\(refresh)"
    }

    /// 已知为零与未报告分别显示；未执行过时显示主会话尚未开启。
    private var contextLine: String {
        guard let usage else { return L10n.tr(model.isOffline ? "Unavailable" : "Loading…") }
        return usage.hasSession ? "\(number(usage.used)) / \(number(usage.size))" : L10n.tr("No active main session.")
    }

    /// 数值按当前区域格式显示。
    private func number(_ value: Int64?) -> String { value?.formatted() ?? L10n.tr("Unavailable") }

    /// 最近尝试时间与模型作为标题，每个用量字段保持独立。
    private func attemptRow(_ attempt: BotSessionUsage.Attempt) -> some View {
        let time = Date(timeIntervalSince1970: Double(attempt.at) / 1000).formatted(date: .numeric, time: .shortened)
        return Disclosure(isExpanded: Binding(get: { expanded == attempt.id }, set: { expanded = $0 ? attempt.id : nil })) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(attempt.rows.indices, id: \.self) { row in
                    let metric = attempt.rows[row]
                    Text("\(L10n.tr(metric.label)) · \(number(metric.value))").font(.footnote)
                }
            }
        } label: {
            Text("\(time) · \(attempt.backend) · \(attempt.model ?? L10n.tr("Unknown model"))").font(.footnote)
        }
    }

    /// 任务状态变化或点击刷新后读取最新快照，网络异常保留上次显示值。
    private func load() async {
        guard !model.isOffline else { return }
        do {
            let snapshot = try await model.botSessionUsage(bot.id)
            guard !Task.isCancelled else { return }
            usage = snapshot
            error = nil
        } catch {
            if !Task.isCancelled { self.error = error.localizedDescription }
        }
    }

    /// 交接已保存才关闭详情，失败允许用户再次尝试。
    private func prepare() async {
        preparing = true
        defer { preparing = false }
        do {
            try await model.prepareHandoff(bot.id)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
