import CodyncKit
import SwiftUI

public extension View {
    /// The one "Delete <bot>?" confirmation, used wherever a bot can be deleted.
    /// Setting `bot` presents it; `onDeleted` runs after the store deletes it.
    func deleteBotConfirmation(_ bot: Binding<Bot?>, onDeleted: @escaping () -> Void = {}) -> some View {
        modifier(DeleteBotConfirmation(bot: bot, onDeleted: onDeleted))
    }

}

private struct DeleteBotConfirmation: ViewModifier {
    @Binding var bot: Bot?
    let onDeleted: () -> Void
    @Environment(BotStore.self) private var model

    /// 展示用户可选的删除范围。较旧的 Host 只显示原操作。
    func body(content: Content) -> some View {
        let isGroup = bot?.isGroup == true
        let canPurge = isGroup && model.supportsGroupPurge
        let message = canPurge
            ? "Full deletion removes the conversation, exclusive bots and their resources. Copied chats, shared bots and external projects stay."
            : isGroup ? "Its bots and their own chats stay." : "Files it changed on your computer stay as they are."
        return content.codyncDialog(
            "Delete \(bot?.name ?? "bot")?",
            isPresented: Binding(get: { bot != nil }, set: { if !$0 { bot = nil } }),
            message: message
        ) {
            // 捕获当前目标，弹窗会在执行动作前清除绑定。
            guard let target = bot else { return [] }
            var actions = [DialogAction(isGroup ? "Delete only group chat" : "Delete bot and its conversation", destructive: true) {
                model.delete(target)
                onDeleted()
            }]
            if canPurge {
                actions.append(DialogAction("Delete group and its bots", destructive: true) {
                    model.delete(target, purgeGroup: true)
                    onDeleted()
                })
            }
            return actions
        }
    }
}
