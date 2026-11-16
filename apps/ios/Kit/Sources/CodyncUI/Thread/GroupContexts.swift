import CodyncKit
import SwiftUI

/// 跨聊天传递私聊交接的精确身份。
struct PrivateGroupHandoffTarget: Equatable {
    let botId: String
    let entryId: String
}

/// 在群成员设置的现有名单显示上下文及交接入口。
struct GroupContexts: View {
    let room: Bot
    let members: [Bot]
    let remove: (String) -> Void
    let canLeaveEditor: () -> Bool
    @State private var snapshot: GroupSessionContexts?
    @State private var inspecting: String?
    @Environment(\.dismissModal) private var dismiss
    @Environment(BotStore.self) private var model
    @State private var edit: Review?
    @State private var preparing: Task<Void, Never>?
    @State private var busy = false
    @State private var error: String?
    @State private var refreshError: String?

    /// 成员增删使用编辑页的暂存名单，会话确认由 Host 处理。
    init(room: Bot, members: [Bot], remove: @escaping (String) -> Void, canLeaveEditor: @escaping () -> Bool) {
        self.room = room
        self.members = members
        self.remove = remove
        self.canLeaveEditor = canLeaveEditor
    }

    private struct Review: Identifiable {
        let id: String
        let botId: String
        let deliveryId: String?
        var text: String
    }

    private var basis: Int { snapshot?.basis ?? 512_000 }

    /// 在成员名单顶端设置全群显示档位。
    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.tr("Member context")).font(.footnote.weight(.semibold))
                    if snapshot?.members.first?.shared == true {
                        Text(L10n.tr("Shares private sessions")).font(.caption2).foregroundStyle(Palette.secondary)
                    }
                }
                Spacer()
                Text(L10n.tr("Display basis")).font(.caption).foregroundStyle(Palette.secondary)
                ForEach([256_000, 512_000, 1_000_000], id: \.self) { value in
                    Button(groupContextCompact(value)) { Task { await setBasis(value) } }
                        .font(.caption.weight(basis == value ? .semibold : .regular))
                        .padding(5)
                        .background(basis == value ? Palette.bubbleAgent : Color.clear, in: Capsule())
                        .accessibilityAddTraits(basis == value ? .isSelected : [])
                        .disabled(busy || model.isOffline)
                }
            }
            ForEach(members) { bot in
                // 同一行提供头像、读数、会话动作和原有移除操作。
                memberRow(bot)
            }
            if snapshot?.members.contains(where: { $0.shared && ($0.recommended || $0.handoff?.phase == "draft") }) == true, !canLeaveEditor() {
                Text(L10n.tr("Save changes")).font(.caption).foregroundStyle(Palette.secondary)
            }
            if let error = error ?? refreshError, edit == nil { Text(error).font(.caption).foregroundStyle(Palette.danger) }
        }
        .groupContextUpdates(room: room, model: model, snapshot: $snapshot, failure: $refreshError)
        .groupContextInspection(snapshot: snapshot, inspecting: $inspecting, model: model)
        .onDisappear {
            // 设置页已关闭时，正在生成的摘要不得再导航到私聊。
            preparing?.cancel()
        }
        .codyncSheet(item: $edit) { draft in
            VStack(spacing: 0) {
                ModalHeader("Open new session")
                CardForm {
                    CardSection(model.bots[draft.botId]?.name, footer: "The new session starts after the current turn.") {
                        TextField(L10n.tr("Summary for the new session"), text: Binding(
                            get: { edit?.text ?? draft.text },
                            set: { edit?.text = $0 }), axis: .vertical).lineLimit(7...15)
                    }
                    if let error { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
                    Button(L10n.tr("Confirm new session")) { Task { await confirm() } }
                        .buttonStyle(.primary).disabled(busy || (edit?.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .background(Palette.background)
        }
    }

    /// 展示成员的用量，普通群的新开入口会转到该成员的私聊。
    private func memberRow(_ bot: Bot) -> some View {
        let member = snapshot?.members.first { $0.botId == bot.id }
        let waiting = ["approved", "switched"].contains(member?.handoff?.phase ?? "")
        let canReview = !waiting && (member?.recommended == true || member?.handoff?.phase == "draft")
        return HStack(alignment: .top, spacing: 9) {
            CharacterAvatar(bot: bot, size: 32, animated: false)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 4) {
                    Text(bot.name).font(.footnote.weight(.semibold)).lineLimit(1)
                    Spacer(minLength: 0)
                    if waiting {
                        Text(L10n.tr("New session after this turn")).font(.caption2).foregroundStyle(Palette.secondary)
                    } else if canReview, let member {
                        Button(L10n.tr("Open new session")) {
                            // 只准备草稿，确认权留在摘要页面。
                            preparing = Task { await open(member) }
                        }
                        .font(.caption2.weight(.semibold))
                        .padding(5)
                        .background(Palette.background, in: RoundedRectangle(cornerRadius: 6))
                        .disabled(busy || model.isOffline || (member.shared && !canLeaveEditor()))
                    }
                }
                Button {
                    // 可从用量读数查看精确 Token 数及实际模型窗口。
                    inspecting = bot.id
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(member?.used.map(groupContextCompact) ?? "—").font(.caption.weight(.semibold))
                            Spacer()
                            Text(groupContextPercent(member?.used, basis) ?? L10n.tr(member?.hasSession == false ? "No session yet" : member == nil ? "Unavailable" : "Usage not reported"))
                                .font(.caption2).foregroundStyle(Palette.secondary)
                        }
                        if let used = member?.used {
                            GeometryReader { geometry in
                                Capsule().fill(Palette.secondary.opacity(0.16)).overlay(alignment: .leading) {
                                    Capsule().fill(AvatarPalette.color(bot.avatarColor))
                                        .frame(width: geometry.size.width * CGFloat(min(1, Double(used) / Double(basis))))
                                }
                            }.frame(height: 3)
                        }
                    }.foregroundStyle(Palette.text)
                }
                .buttonStyle(.plain)
                .disabled(member == nil)
            }
            IconButton("Remove \(bot.name)", systemImage: "xmark") {
                // 移除仍然只编辑尚未保存的成员名单。
                remove(bot.id)
            }
        }
        .padding(8)
        .background(canReview ? Palette.bubbleAgent : Color.clear, in: RoundedRectangle(cornerRadius: 10))
    }

    /// 更新群共享档位，群修订会同步聊天名字旁的占比。
    private func setBasis(_ value: Int) async {
        busy = true
        defer { busy = false }
        do {
            struct Input: Encodable { let roomId: String; let basis: Int }
            snapshot = try await model.ready().call("setGroupContextBasis", Input(roomId: room.id, basis: value))
            error = nil
        } catch { self.error = error.localizedDescription }
    }

    /// 已有阻塞任务沿用原交接，共享摘要只在正确的私聊打开。
    private func open(_ member: GroupSessionContexts.Member) async {
        guard !busy, !member.shared || canLeaveEditor() else { return }
        busy = true
        defer { busy = false }
        do {
            let response: GroupSessionContexts.Review = try await model.ready()
                .call("prepareGroupMemberSession", ["roomId": room.id, "botId": member.botId])
            guard !Task.isCancelled else { return }
            error = nil
            if let id = response.privateEntryId {
                // 等待请求期间也可能编辑群资料，离开前再读一次暂存状态。
                guard canLeaveEditor() else { return }
                dismiss()
                model.privateGroupHandoff = PrivateGroupHandoffTarget(botId: member.botId, entryId: id)
                model.selection = member.botId
            } else if let delivery = response.deliveryId {
                let draft = try await model.groupHandoff(delivery)
                guard !Task.isCancelled else { return }
                edit = Review(id: draft.revision, botId: member.botId, deliveryId: delivery, text: draft.text)
            } else if let text = response.text, let rev = response.revision {
                edit = Review(id: rev, botId: member.botId, deliveryId: nil, text: text)
            }
        } catch { if !Task.isCancelled { self.error = error.localizedDescription } }
    }

    /// 确认成功才关闭编辑面板，保存失败可保留输入后重试。
    private func confirm() async {
        guard let edit else { return }
        busy = true
        defer { busy = false }
        do {
            if let delivery = edit.deliveryId {
                try await model.confirmGroupHandoff(delivery, revision: edit.id, text: edit.text)
            } else {
                let _: Empty = try await model.ready().call("confirmGroupMemberSession",
                    ["roomId": room.id, "botId": edit.botId, "revision": edit.id, "text": edit.text])
            }
            self.edit = nil
        } catch { self.error = error.localizedDescription }
    }
}

extension View {
    /// 监听当前群和成员的修订，使聊天标签在成员页面关闭时也能实时更新。
    func groupContextUpdates(room: Bot?, model: BotStore, snapshot: Binding<GroupSessionContexts?>,
                             failure: Binding<String?>) -> some View {
        let ready = room?.isGroup == true && model.hello?.groupSessionContexts == true && !model.isOffline && model.mismatch == nil
        let revisions = room.map { bot in
            // 合并成员、群和连接状态，重连时重新同步。
            model.members(of: bot).map { "\($0.id):\($0.rev)" }.joined(separator: "|")
        } ?? ""
        return task(id: "\(room?.id ?? ""):\(room?.rev ?? 0):\(revisions):\(ready)") {
            guard let room, ready else { return }
            do {
                try await Task.sleep(for: .milliseconds(90))
                let value: GroupSessionContexts = try await model.ready().call("groupSessionContexts", ["roomId": room.id])
                guard !Task.isCancelled else { return }
                snapshot.wrappedValue = value
                failure.wrappedValue = nil
            } catch {
                if !Task.isCancelled { failure.wrappedValue = error.localizedDescription }
            }
        }
    }

    /// 从聊天标签或成员行都能查看同一套精确读数。
    func groupContextInspection(snapshot: GroupSessionContexts?, inspecting: Binding<String?>, model: BotStore) -> some View {
        let presentation = Binding(get: {
            // 存在有效目标才呈现详情。
            inspecting.wrappedValue != nil
        }, set: { shown in
            if !shown { inspecting.wrappedValue = nil }
        })
        return codyncSheet(isPresented: presentation) {
            if let botId = inspecting.wrappedValue, let member = snapshot?.members.first(where: { $0.botId == botId }) {
                let basis = snapshot?.basis ?? 512_000
                VStack(spacing: 0) {
                    ModalHeader(model.bots[botId]?.name ?? "Member context")
                    CardForm {
                        CardSection("Current context") {
                            Text(member.used.map { "\($0.formatted()) token" } ?? L10n.tr(member.hasSession ? "Usage not reported" : "No session yet"))
                            Text("\(L10n.tr("Display basis")) · \(groupContextCompact(basis)) · \(groupContextPercent(member.used, basis) ?? "—")")
                            Text("\(L10n.tr("Actual model window")) · \(member.size.map(groupContextCompact) ?? "—") · \(member.size.flatMap { groupContextPercent(member.used, $0) } ?? "—")")
                            if member.shared { Text(L10n.tr("This also starts a new private session")) }
                        }
                    }
                }
                .background(Palette.background)
            }
        }
    }
}

/// 固定档位和 token 用量共用十进制单位。
func groupContextCompact(_ amount: Int) -> String {
    if amount == 1_000_000 { return "1M" }
    return (Double(amount) / 1000).formatted(.number.precision(.fractionLength(0...1))) + "K"
}

/// 百分比跟随选中档位，实际窗口未知时不除零。
func groupContextPercent(_ used: Int?, _ size: Int) -> String? {
    guard let used, size > 0 else { return nil }
    return (Double(used) / Double(size)).formatted(.percent.precision(.fractionLength(1)))
}

/// 从群跳入私聊时定位到原卡，也能打开尚未加载的历史卡。
struct PrivateGroupHandoffReview: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    /// 同步编辑和确认后的卡片修订。
    var body: some View {
        let latest = model.chat(entry.botId).first { $0.id == entry.id } ?? entry
        VStack(alignment: .leading, spacing: 12) {
            ModalHeader("Open new session")
            if let card = latest.data.handoff {
                ScrollView { HandoffCardRow(entry: latest, card: card).padding(12) }
            }
        }
        .background(Palette.background)
    }
}
