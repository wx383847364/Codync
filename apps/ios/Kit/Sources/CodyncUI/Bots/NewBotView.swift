import CodyncKit
import SwiftUI

/// A new bot, profile first (Grok Bot's Create New Bot): its avatar and name. Everything else
/// starts on the usual defaults and is changed afterwards in the bot's Settings.
public struct NewBotView: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    @State private var draft = BotDraft()
    @State private var saving = false
    @State private var error: String?
    private let computers: [(id: ComputerID, label: String)]
    @Binding private var computer: ComputerID

    /// `computers` can take the bot; the caller swaps the `BotStore` to match `computer`.
    public init(computers: [(id: ComputerID, label: String)], computer: Binding<ComputerID>) {
        self.computers = computers
        _computer = computer
    }

    public var body: some View {
        CreateForm(title: "New bot", placeholder: "Name your bot", name: $draft.name, saving: saving,
                   canCreate: !saving && model.connection == .online, error: error, create: save) {
            CharacterAvatar(shape: draft.avatarShape, color: draft.avatarColor, size: 96)
        } content: {
            AvatarPicker(shape: $draft.avatarShape, color: $draft.avatarColor)
            if computers.count > 1 || model.connection != .online {
                VStack(alignment: .leading, spacing: 6) {
                    if computers.count > 1 {
                        ValueRow("Computer") {
                            ChoicePicker(selection: $computer, options: computers, fitsAvailableWidth: true)
                                .accessibilityLabel("Computer")
                        }
                    }
                    if model.connection != .online {
                        Text(computers.count > 1
                            ? "Connect this computer to create the bot, or choose another computer."
                            : "Connect this computer to create the bot.")
                            .font(.caption).foregroundStyle(Palette.warning)
                    }
                }
            }
        }
    }

    private func save() {
        guard !saving, model.connection == .online else { return }
        saving = true
        error = nil
        var draft = draft
        draft.name = draft.name.trimmingCharacters(in: .whitespacesAndNewlines)
        model.fillDefaults(&draft)
        Task {
            do {
                let bot = try await model.save(draft)
                dismiss()
                model.selection = bot.id
            } catch {
                withAnimation(Motion.fade) { self.error = error.localizedDescription }
            }
            saving = false
        }
    }
}

private extension BotStore {
    /// Picks an installed agent we know (a registry agent nobody chose is never the default);
    /// empty cwd requests a personal workspace from the host. Connectors and skills stay unset:
    /// the host turns on every one it can run.
    func fillDefaults(_ draft: inout BotDraft) {
        if hello?.backends.first(where: { $0.id == draft.backend })?.available != true,
           let first = hello?.backends.first(where: { $0.available && $0.curated == true }) {
            draft.backend = first.id
        }
    }
}

/// 建群与群内新会话共用表单，来源群用于预填可调整的成员。
public struct NewGroupView: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    private let source: Bot?
    @State private var name = ""
    @State private var members: [String]
    @State private var leader: String?
    @State private var saving = false
    @State private var error: String?

    /// 新建群从空名单开始，新会话预填来源群的成员和负责人。
    public init(source: Bot? = nil) {
        self.source = source
        _members = State(initialValue: source?.members ?? [])
        _leader = State(initialValue: source?.stage?.leaderId ?? source?.groupLeaderId)
    }

    private var picked: [Bot] { members.compactMap { model.bots[$0] }.filter { !$0.isGroup } }
    private var defaultName: String { picked.map(\.name).joined(separator: ", ") }
    private var selectedLeader: Bot? { picked.first { $0.id == leader } }

    /// 与阶段群使用相同顺序放置搜索、成员和负责人下拉。
    public var body: some View {
        CreateForm(title: source == nil ? "New group chat" : "Create new group session",
                   placeholder: picked.isEmpty ? "Name your group" : defaultName, name: $name,
                   saving: saving, canCreate: !picked.isEmpty && !saving, error: error, create: save,
                   group: true, displayName: source.map { L10n.tr("{name} · Auto-numbered").replacingOccurrences(of: "{name}", with: $0.name) }) {
            GroupAvatar(members: picked, size: 80).animation(Motion.layout, value: members)
        } content: {
            let visible = model.roster.filter { !$0.isGroup }
            let ids = Set(visible.map(\.id))
            let inherited = source.map { model.members(of: $0) } ?? []
            VStack(alignment: .leading, spacing: 12) {
                GroupMemberPicker(bots: visible + inherited.filter { !ids.contains($0.id) }, picked: picked, toggle: toggle)
                if model.supportsGroupMemberPicker {
                    GroupBotPicker(bots: picked, selection: $leader, label: "Group lead (optional)", allowsNone: true)
                }
                Text(source != nil ? L10n.tr("Original members are preselected. You can change them.")
                     : selectedLeader.map { L10n.tr("Ask {name} · @ to choose another recipient").replacingOccurrences(of: "{name}", with: $0.name) }
                     ?? L10n.tr("Ask everyone · @ to ask one bot"))
                    .font(.caption).foregroundStyle(Palette.secondary)
            }
        }
    }

    /// 移除已选负责人时同时清空其身份。
    private func toggle(_ bot: Bot) {
        withAnimation(Motion.layout) {
            if members.contains(bot.id) {
                members.removeAll { $0 == bot.id }
                if leader == bot.id { leader = nil }
            } else {
                members.append(bot.id)
            }
        }
    }

    /// 固定本次提交的名单，成功后打开新聊天。
    private func save() {
        let ids = picked.map(\.id)
        guard !ids.isEmpty, !saving else { return }
        saving = true
        error = nil
        let typed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let finalName = typed.isEmpty ? defaultName : typed
        let groupLeaderId = model.supportsGroupMemberPicker ? selectedLeader?.id : nil
        Task {
            defer { saving = false }
            do {
                if let source {
                    try await model.saveGroupSession(source.id, members: ids, groupLeaderId: groupLeaderId)
                } else {
                    _ = try await model.createGroup(name: finalName, members: ids, groupLeaderId: groupLeaderId)
                }
                dismiss()
            } catch {
                withAnimation(Motion.fade) { self.error = error.localizedDescription }
            }
        }
    }
}

/// 根据查询显示可滚动的候选名单，头像网格仅显示已加入成员。
struct GroupMemberPicker: View {
    let bots: [Bot]
    let picked: [Bot]
    let toggle: (Bot) -> Void
    @State private var query = ""
    @State private var open = false

    /// 搜索和浏览复用同一组候选，选中状态由表单提供。
    var body: some View {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let results = bots.filter { needle.isEmpty || $0.name.localizedCaseInsensitiveContains(needle) }
        let chosen = Set(picked.map(\.id))
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Palette.secondary)
                TextField(L10n.tr("Search or add bots"), text: $query)
                    .autocorrectionDisabled()
                    .onChange(of: query) { _, _ in
                        // 输入及清空时同步更新展开名单。
                        withAnimation(Motion.layout) { open = true }
                    }
                Button {
                    // 箭头重新打开时浏览完整名单。
                    if !open { query = "" }
                    withAnimation(Motion.layout) { open.toggle() }
                } label: {
                    Image(systemName: open ? "chevron.up" : "chevron.down").frame(minWidth: 28, minHeight: 26)
                }.buttonStyle(.plain).accessibilityLabel(L10n.tr("Show bot list"))
            }.fieldBox(fill: Palette.surface)
            if open {
                GroupBotOptions(bots: results, selected: chosen, multiple: true) { bot in
                    // 添加列表只切换成员，不关闭名单。
                    if let bot { toggle(bot) }
                }
            }
            if !picked.isEmpty {
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 4), spacing: 8) {
                    ForEach(picked) { bot in
                        // 点击已加入的头像可移除该成员。
                        Button { toggle(bot) } label: {
                            VStack(spacing: 4) {
                                CharacterAvatar(bot: bot, size: 44, animated: false).choiceRing(true)
                                Text(bot.name).font(.caption).lineLimit(1)
                            }.frame(maxWidth: .infinity)
                        }.buttonStyle(.plain)
                            .accessibilityLabel(L10n.tr("Remove {name}").replacingOccurrences(of: "{name}", with: bot.name))
                    }
                }
            }
        }
    }
}

/// 三种下拉共用全宽头像行，只有多选成员时显示加号。
private struct GroupBotOptions: View {
    let bots: [Bot]
    let selected: Set<String>
    var multiple = false
    var allowsNone = false
    let pick: (Bot?) -> Void

    /// 保留列表内滚动，选项不会超过五行的高度。
    var body: some View {
        if bots.isEmpty && !allowsNone {
            Text(L10n.tr("No matching bots")).font(.caption).foregroundStyle(Palette.secondary)
        } else {
            ScrollView {
                LazyVStack(spacing: 0) {
                    if allowsNone { option(nil, on: selected.isEmpty) }
                    ForEach(bots) { bot in
                        option(bot, on: selected.contains(bot.id))
                    }
                }
            }
            .frame(height: CGFloat(min(bots.count + (allowsNone ? 1 : 0), 5) * 44))
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))
        }
    }

    /// 无负责人的选项留出头像宽度，所有名称保持左对齐。
    private func option(_ bot: Bot?, on: Bool) -> some View {
        Button { pick(bot) } label: {
            HStack(spacing: 8) {
                if let bot {
                    CharacterAvatar(bot: bot, size: 24, animated: false)
                } else {
                    Color.clear.frame(width: 24, height: 24)
                }
                Text(bot?.name ?? L10n.tr("No lead")).lineLimit(1)
                Spacer()
                if on || multiple { Image(systemName: on ? "checkmark" : "plus") }
            }.padding(.horizontal, 10).frame(height: 44).contentShape(Rectangle())
        }.buttonStyle(.plain).accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// 负责人和编辑角色的单选列表在触发控件下方展开。
struct GroupBotPicker: View {
    let bots: [Bot]
    @Binding var selection: String?
    let label: String
    var allowsNone = false
    var showsLabel = true
    @State private var open = false

    /// 选定后收起列表，让用户继续填写表单。
    var body: some View {
        let current = bots.first { $0.id == selection }
        VStack(spacing: 8) {
            Button {
                withAnimation(Motion.layout) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    if showsLabel {
                        Text(L10n.tr(label))
                        Spacer(minLength: 0)
                    }
                    Text(current?.name ?? L10n.tr(allowsNone ? "No lead" : "Choose")).lineLimit(1)
                    if !showsLabel { Spacer(minLength: 0) }
                    Image(systemName: open ? "chevron.up" : "chevron.down").font(.caption)
                }.fieldBox(fill: Palette.surface)
            }.buttonStyle(.plain).disabled(bots.isEmpty)
                .accessibilityLabel(L10n.tr(label))
                .accessibilityValue(current?.name ?? L10n.tr(allowsNone ? "No lead" : "Choose"))
            if open {
                GroupBotOptions(bots: bots, selected: Set(current.map { [$0.id] } ?? []), allowsNone: allowsNone) { bot in
                    withAnimation(Motion.layout) {
                        selection = bot?.id
                        open = false
                    }
                }
            }
        }
    }
}

/// The create dialogs' layout: a large preview, a centered name field (Return creates), the
/// pickers, then any save error.
struct CreateForm<Preview: View, Content: View>: View {
    let title: String
    let placeholder: String
    @Binding var name: String
    let saving: Bool
    let canCreate: Bool
    let error: String?
    let create: () -> Void
    var group = false
    var displayName: String? = nil
    @ViewBuilder let preview: Preview
    @ViewBuilder let content: Content
    @FocusState private var naming: Bool

    /// 初始化共用表单，群会话可展示由 Host 编号的只读名称。
    init(title: String, placeholder: String, name: Binding<String>, saving: Bool, canCreate: Bool,
         error: String?, create: @escaping () -> Void, group: Bool = false, displayName: String? = nil,
         @ViewBuilder preview: () -> Preview, @ViewBuilder content: () -> Content) {
        self.title = title
        self.placeholder = placeholder
        _name = name
        self.saving = saving
        self.canCreate = canCreate
        self.error = error
        self.create = create
        self.group = group
        self.displayName = displayName
        self.preview = preview()
        self.content = content()
    }

    /// 展示预览和表单字段，并用顶部操作确认创建。
    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(title) {
                if saving {
                    Spinner()
                } else {
                    IconButton("Create", systemImage: "checkmark", action: create)
                        .disabled(!canCreate)
                }
            }
            ScrollView {
                VStack(spacing: group ? 12 : 28) {
                    VStack(spacing: group ? 12 : 16) {
                        preview
                        if let displayName {
                            Text(displayName).frame(maxWidth: .infinity, alignment: .leading).fieldBox(fill: Palette.surface)
                        } else if group {
                            TextField(L10n.tr(placeholder), text: $name).fieldBox(fill: Palette.surface)
                                .submitLabel(.done)
                                .focused($naming)
                                .onSubmit {
                                    // 只有满足表单要求时才接受键盘确认。
                                    if canCreate && !saving { create() }
                                }
                        } else {
                            TextField(L10n.tr(placeholder), text: $name)
                                .textFieldStyle(.plain)
                                .font(.title2.weight(.semibold))
                                .foregroundStyle(Palette.text)
                                .multilineTextAlignment(.center)
                                .submitLabel(.done)
                                .focused($naming)
                                .onSubmit(create)
                        }
                    }
                    content
                    if let error {
                        Text(L10n.tr(error))
                            .font(.footnote)
                            .foregroundStyle(Palette.danger)
                            .multilineTextAlignment(.center)
                            .transition(.opacity)
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.horizontal, 20)
                .padding(.top, 8)
                .padding(.bottom, 24)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .background(Palette.background)
        .onAppear { naming = displayName == nil }
    }
}
