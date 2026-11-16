import CodyncKit
import SwiftUI

/// 跨聊天传递私聊交接的精确身份。
struct PrivateGroupHandoffTarget: Equatable {
    let botId: String
    let entryId: String
}

/// 群顶部用量和交接入口，窄屏可以横向浏览所有成员。
struct GroupContexts: View {
    let room: Bot
    @Binding var snapshot: GroupSessionContexts?
    @Binding var inspecting: String?
    @Environment(BotStore.self) private var model
    @State private var edit: Review?
    @State private var busy = false
    @State private var error: String?
    @State private var refreshError: String?

    private struct Review: Identifiable {
        let id: String
        let botId: String
        let deliveryId: String?
        var text: String
    }

    private var basis: Int { snapshot?.basis ?? 512_000 }

    /// 房间或成员更新时合并取数，旧响应不得覆盖新房间。
    private var revision: String {
        let members = model.members(of: room).map { "\($0.id):\($0.rev)" }.joined(separator: "|")
        return "\(room.id):\(room.rev):\(members):\(model.isOffline)"
    }

    /// 所有成员的读数保留在一行，显示基准和操作属于当前群。
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
            ScrollView(.horizontal) {
                HStack(spacing: 7) {
                    ForEach(model.members(of: room)) { bot in card(bot) }
                }
            }
            .scrollIndicators(.hidden)
            if let error = error ?? refreshError, edit == nil { Text(error).font(.caption).foregroundStyle(Palette.danger) }
        }
        .padding(.horizontal, 13).padding(.vertical, 6)
        .background(Palette.background)
        .task(id: revision) { await refresh() }
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
        .codyncSheet(isPresented: Binding(get: { inspecting != nil }, set: { if !$0 { inspecting = nil } })) {
            if let inspecting, let member = snapshot?.members.first(where: { $0.botId == inspecting }) {
                VStack(spacing: 0) {
                    ModalHeader(model.bots[inspecting]?.name ?? "Member context")
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

    /// 卡片在右上角提供动作，用量区域单独打开详情。
    private func card(_ bot: Bot) -> some View {
        let member = snapshot?.members.first { $0.botId == bot.id }
        return VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 4) {
                CharacterAvatar(bot: bot, size: 19, animated: false)
                Text(bot.name).font(.caption.weight(.semibold)).lineLimit(1)
                Spacer(minLength: 3)
                if let member {
                    if ["approved", "switched"].contains(member.handoff?.phase ?? "") {
                        Text(L10n.tr("New session after this turn")).font(.caption2).foregroundStyle(Palette.secondary)
                    } else if member.recommended || member.handoff?.phase == "draft" {
                        Button(L10n.tr("Open new session")) { Task { await open(member) } }
                            .font(.caption2.weight(.semibold))
                            .disabled(busy || model.isOffline)
                    }
                }
            }
            Button {
                if member != nil { inspecting = bot.id }
            } label: {
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(member?.used.map(groupContextCompact) ?? "—").font(.subheadline.weight(.semibold))
                        Spacer()
                        Text(groupContextPercent(member?.used, basis) ?? L10n.tr(member?.hasSession == false ? "No session yet" : "Usage not reported"))
                            .font(.caption2).foregroundStyle(Palette.secondary)
                    }
                    GeometryReader { geometry in
                        Capsule().fill(Palette.secondary.opacity(0.16)).overlay(alignment: .leading) {
                            Capsule().fill(AvatarPalette.color(bot.avatarColor))
                                .frame(width: geometry.size.width * CGFloat(min(1, Double(member?.used ?? 0) / Double(basis))))
                        }
                    }.frame(height: 3)
                }.foregroundStyle(Palette.text)
            }
            .buttonStyle(.plain)
        }
        .frame(width: 181)
        .padding(8)
        .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 12))
    }

    /// 只在在线时读取并忽略已取消的刷新。
    private func refresh() async {
        guard !model.isOffline else { return }
        do {
            try await Task.sleep(for: .milliseconds(90))
            let value: GroupSessionContexts = try await model.ready().call("groupSessionContexts", ["roomId": room.id])
            if !Task.isCancelled { snapshot = value; refreshError = nil }
        } catch is CancellationError {
        } catch { refreshError = error.localizedDescription }
    }

    /// 更新群共享档位，响应立即更新顶部和名字旁。
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
        busy = true
        defer { busy = false }
        do {
            let response: GroupSessionContexts.Review = try await model.ready()
                .call("prepareGroupMemberSession", ["roomId": room.id, "botId": member.botId])
            error = nil
            if let id = response.privateEntryId {
                model.privateGroupHandoff = PrivateGroupHandoffTarget(botId: member.botId, entryId: id)
                model.selection = member.botId
            } else if let delivery = response.deliveryId {
                let draft = try await model.groupHandoff(delivery)
                edit = Review(id: draft.revision, botId: member.botId, deliveryId: delivery, text: draft.text)
            } else if let text = response.text, let rev = response.revision {
                edit = Review(id: rev, botId: member.botId, deliveryId: nil, text: text)
            }
        } catch { self.error = error.localizedDescription }
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
