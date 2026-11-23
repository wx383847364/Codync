import CodyncKit
import SwiftUI

/// Change a group chat's name, what it's for and who's in it. Bots are picked like
/// recipients: chips on top, a search, and the bots not in it yet.
public struct GroupEditorView: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    @State private var initial: Bot
    private let isPresented: () -> Bool
    /// 会话跳转前核对本页的草稿，避免丢掉尚未保存的修改。
    private var groupId: String { initial.id }
    /// 滑动关闭后也禁止晚到的请求跳转。
    private var canLeaveEditor: Bool {
        isPresented() && !saving && name == initial.name && about == initial.description && members == initial.members
    }
    @State private var name: String
    @State private var about: String
    @State private var members: [String]
    @State private var query = ""
    @State private var adding = false
    @State private var saving = false
    @State private var error: String?
    @State private var confirmDelete: Bot?

    /// 由呈现端提供关闭状态，使关闭动画中的请求也不能触发跳转。
    public init(group: Bot, isPresented: @escaping () -> Bool = { true }) {
        _initial = State(initialValue: group)
        self.isPresented = isPresented
        _name = State(initialValue: group.name)
        _about = State(initialValue: group.description)
        _members = State(initialValue: group.members)
    }

    private var picked: [Bot] { members.compactMap { model.bots[$0] } }
    /// Bots that can still join, filtered by the search.
    private var candidates: [Bot] {
        let q = query.trimmingCharacters(in: .whitespaces)
        return model.roster.filter { !$0.isGroup && !members.contains($0.id) && (q.isEmpty || $0.name.localizedCaseInsensitiveContains(q)) }
    }
    private var canSave: Bool { !members.isEmpty && !saving }

    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader("Group chat") {
                if saving {
                    Spinner()
                } else {
                    IconButton("Save", systemImage: "checkmark") { save() }
                        .disabled(!canSave)
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 28) {
                    VStack(spacing: 10) {
                        GroupAvatar(members: picked, size: 84)
                            .animation(Motion.layout, value: members)
                        TextField(defaultName, text: $name)
                            .textFieldStyle(.plain)
                            .font(.title2.weight(.semibold))
                            .foregroundStyle(Palette.text)
                            .multilineTextAlignment(.center)
                            .padding(.top, 6)
                        TextField("What this group works on", text: $about, axis: .vertical)
                            .textFieldStyle(.plain)
                            .lineLimit(1...4)
                            .font(.subheadline)
                            .foregroundStyle(Palette.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    VStack(alignment: .leading, spacing: 0) {
                        let room = model.bots[groupId]
                        if let room, model.hello?.groupSessionContexts == true {
                            // 用量直接进入已有成员名单，新增和移除仍由这个编辑页处理。
                            GroupContexts(room: room, members: picked, remove: toggle, canLeaveEditor: { canLeaveEditor })
                        } else {
                            Text(L10n.tr("Members"))
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(Palette.secondary)
                                .padding(.bottom, 4)
                            ForEach(picked) { bot in
                                memberRow(bot) {
                                    IconButton("Remove \(bot.name)", systemImage: "xmark") { toggle(bot.id) }
                                }
                                .transition(.opacity)
                            }
                        }
                        if adding {
                            TextField("Search bots", text: $query)
                                .fieldBox(fill: Palette.surface)
                                .onSubmit { if let first = candidates.first { toggle(first.id) } }
                                .padding(.vertical, 6)
                            ForEach(candidates) { bot in
                                Button { toggle(bot.id) } label: {
                                    memberRow(bot, dimmed: true) {
                                        Image(systemName: "plus.circle").font(.system(size: 18)).foregroundStyle(Palette.secondary)
                                    }
                                }
                                .buttonStyle(PressScale())
                                .accessibilityLabel("Add \(bot.name)")
                            }
                        } else if !candidates.isEmpty {
                            Button { withAnimation(Motion.layout) { adding = true } } label: {
                                HStack(spacing: 14) {
                                    Image(systemName: "plus").font(.system(size: 16)).foregroundStyle(Palette.secondary).frame(width: 32)
                                    Text(L10n.tr("Add Member")).foregroundStyle(Palette.secondary)
                                    Spacer(minLength: 0)
                                }
                                .frame(minHeight: 48)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(PressScale())
                        }
                        let lead = picked.first { $0.id == (room?.stage?.leaderId ?? room?.groupLeaderId) }
                        Text(lead.map { L10n.tr("Ask {name} · @ to choose another recipient").replacingOccurrences(of: "{name}", with: $0.name) }
                             ?? L10n.tr(room?.stage?.leaderId.isEmpty == false ? "Choose a recipient with @" : "Everyone answers in turn unless you @mention someone."))
                            .font(.caption)
                            .foregroundStyle(Palette.tertiary)
                            .padding(.top, 10)
                    }
                    .animation(Motion.layout, value: members)
                    if model.hello?.groupResourceUsage == true {
                        GroupResourceUsageView(roomId: groupId)
                    }
                    if let error {
                        Text(L10n.tr(error)).font(.footnote).foregroundStyle(Palette.danger).transition(.opacity)
                    }
                    if let group = model.bots[groupId] {
                        Button(L10n.tr("Delete group chat"), role: .destructive) { confirmDelete = group }
                            .foregroundStyle(Palette.danger)
                            .buttonStyle(PressScale())
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 8)
                .padding(.bottom, 24)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .background(Palette.background)
        .deleteBotConfirmation($confirmDelete) { dismiss() }
    }

    /// "Alice, Bob" when no name is typed.
    private var defaultName: String {
        let names = picked.map(\.name)
        return names.isEmpty ? "Name" : names.joined(separator: ", ")
    }

    private func toggle(_ id: String) {
        withAnimation(Motion.layout) {
            if members.contains(id) { members.removeAll { $0 == id } } else { members.append(id) }
        }
        query = ""
    }

    /// A member or candidate: avatar, name, and a trailing control (Grok Bot's member list).
    private func memberRow(_ bot: Bot, dimmed: Bool = false, @ViewBuilder trailing: () -> some View) -> some View {
        HStack(spacing: 14) {
            CharacterAvatar(bot: bot, size: 32, animated: false)
            Text(bot.name).foregroundStyle(dimmed ? Palette.secondary : Palette.text).lineLimit(1)
            Spacer(minLength: 0)
            trailing()
        }
        .frame(minHeight: 48)
        .contentShape(Rectangle())
    }

    private func save() {
        saving = true
        error = nil
        let typed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let finalName = typed.isEmpty ? defaultName : typed
        let about = about.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            do {
                var draft = GroupDraft(name: finalName, description: about, members: members)
                draft.id = groupId
                draft.pinned = model.bots[groupId]?.pinned
                try await model.updateGroup(draft)
                dismiss()
            } catch {
                withAnimation(Motion.fade) { self.error = error.localizedDescription }
            }
            saving = false
        }
    }
}

/// 手机显示 Host 汇总，文件目录在运行 Host 的桌面上打开。
private struct GroupResourceUsageView: View {
    private struct Usage: Decodable {
        let resourceBytes: Int64?
        let groupTokens: Int64?
        let hostTokens: Int64?
        let share: Double?
        let partial: Bool
    }

    @Environment(BotStore.self) private var model
    @State private var usage: Usage?
    @State private var failed = false
    let roomId: String

    /// 切换 Host 连接状态会重读统计，取消请求时保留现有读数。
    private var online: Bool { !model.isOffline && model.mismatch == nil }

    /// 在成员下方显示资源占用、本群累计、占比和 Host 总量。
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(L10n.tr("Resources and usage")).font(.subheadline.weight(.semibold))
            if let usage {
                reading("Group attachments and shared files", usage.resourceBytes.map {
                    ByteCountFormatter.string(fromByteCount: $0, countStyle: .file)
                } ?? L10n.tr("Unavailable"), icon: "folder")
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .background(Palette.surface, in: RoundedRectangle(cornerRadius: 13))
            }
            if model.hello?.groupWorkspaceUsage == true {
                GroupWorkspaceUsageView(roomId: roomId, online: online, members: model.bots[roomId]?.members ?? [])
            }
            VStack(alignment: .leading, spacing: 10) {
                if let usage {
                    HStack(alignment: .top) {
                        reading("Group token usage", usage.groupTokens.map { "\($0.formatted()) Token" }
                            ?? L10n.tr("No usage recorded"), icon: "chart.bar")
                        Spacer(minLength: 4)
                        if let share = usage.share {
                            VStack(alignment: .trailing, spacing: 2) {
                                Text(share.formatted(.percent.precision(.fractionLength(0...1)))).fontWeight(.semibold)
                                Text(L10n.tr("Share of host usage")).font(.caption).foregroundStyle(Palette.secondary)
                            }
                        }
                    }
                    if let share = usage.share {
                        Capsule().fill(Palette.bubbleUser).overlay(alignment: .leading) {
                            // 比例由 Host 计算，进度条只负责绘制。
                            GeometryReader { proxy in
                                Capsule().fill(Color.blue).frame(width: proxy.size.width * share)
                            }
                        }
                        .frame(height: 5)
                        .accessibilityHidden(true)
                    }
                    if let total = usage.hostTokens {
                        Text(L10n.tr("All conversations on this host: {tokens} Token")
                            .replacingOccurrences(of: "{tokens}", with: total.formatted()))
                            .font(.caption).foregroundStyle(Palette.secondary)
                    }
                    if usage.partial {
                        Text(L10n.tr("Partial usage records")).font(.caption).foregroundStyle(Palette.secondary)
                    }
                    if failed || !online {
                        Text(L10n.tr("Could not refresh usage.")).font(.caption).foregroundStyle(Palette.secondary)
                    }
                } else {
                    Text(L10n.tr(failed || !online ? "Unavailable" : "Loading…"))
                        .font(.footnote).foregroundStyle(Palette.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 13))
        }
        .task(id: online) { await load() }
    }

    /// 同一行组合图标、标签和数值。
    private func reading(_ label: String, _ value: String, icon: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon).frame(width: 28).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.tr(label)).font(.footnote)
                Text(value).font(.callout.weight(.semibold)).monospacedDigit()
            }
        }
    }

    /// 离开群资料页时忽略已取消的读取。
    private func load() async {
        guard online else { return }
        do {
            let result: Usage = try await model.ready().call("groupResourceUsage", ["roomId": roomId])
            guard !Task.isCancelled else { return }
            usage = result
            failed = false
        } catch {
            if !Task.isCancelled { failed = true }
        }
    }
}


/// 显示成员托管目录的占用。手机只读统计，目录入口由本机桌面提供。
private struct GroupWorkspaceUsageView: View {
    private struct Usage: Decodable {
        struct Member: Decodable, Identifiable {
            let botId: String
            let name: String
            let bytes: Int64?
            let shared: Bool
            /// 列表以 Host 的 Bot 身份为键，避免同名成员串行。
            var id: String { botId }
        }
        let totalBytes: Int64?
        let members: [Member]
    }

    @Environment(BotStore.self) private var model
    @State private var usage: Usage?
    @State private var failed = false
    @State private var expanded = false
    @ScaledMetric(relativeTo: .footnote) private var rowHeight: CGFloat = 44
    let roomId: String
    let online: Bool
    let members: [String]

    /// 默认显示合计。展开后可在有限高度内浏览成员。
    var body: some View {
        Disclosure(isExpanded: $expanded) {
            if let rows = usage?.members, !rows.isEmpty {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        // 使用 Host 给出的排序和共享状态。
                        ForEach(rows) { member in
                            Divider()
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(member.name).lineLimit(1)
                                    if member.shared {
                                        Text(L10n.tr("Shared across groups")).font(.caption2).foregroundStyle(Palette.secondary)
                                    }
                                }
                                Spacer(minLength: 8)
                                Text(size(member.bytes)).monospacedDigit()
                            }
                            .font(.footnote)
                            .frame(minHeight: rowHeight - 1)
                        }
                    }
                }
                .frame(height: min(176, CGFloat(rows.count) * rowHeight))
                .scrollBounceBehavior(.basedOnSize)
                .accessibilityIdentifier("member-workspace-list")
            }
        } label: {
            Image(systemName: "folder").frame(width: 28).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.tr("Member workspaces")).font(.footnote.weight(.semibold))
                Text(L10n.tr("Includes project files and build caches")).font(.caption2).foregroundStyle(Palette.secondary)
            }
            Spacer(minLength: 4)
            Text(usage == nil && online && !failed ? L10n.tr("Loading…") : size(usage?.totalBytes))
                .font(.footnote.weight(.semibold)).monospacedDigit()
        }
        .padding(12)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 13))
        // 成员或连接状态变化才请求，Host 对重复读取复用扫描数据。
        .task(id: [online.description] + members) { await load() }
    }

    /// 统一处理未知读数和文件容量单位。
    private func size(_ bytes: Int64?) -> String {
        guard let bytes else { return L10n.tr("Unavailable") }
        return ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    /// 旧连接的响应不会覆盖新名单。
    private func load() async {
        // 名单变化时撤下旧读数，避免失败后留着已退出的成员。
        usage = nil
        failed = false
        guard online else { return }
        do {
            let result: Usage = try await model.ready().call("groupWorkspaceUsage", ["roomId": roomId])
            if !Task.isCancelled {
                usage = result
                failed = false
            }
        } catch {
            if !Task.isCancelled { failed = true }
        }
    }
}
