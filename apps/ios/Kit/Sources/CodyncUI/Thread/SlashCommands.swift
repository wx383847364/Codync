import CodyncKit

/// A command in the composer's `/` list.
struct SlashCommand: Identifiable, Equatable {
    enum Action: Hashable { case fast, skill(String), session(String) }

    /// What follows the `/`.
    let name: String
    let icon: String
    let title: String
    let detail: String
    let warning: Bool
    let action: Action

    var id: Action { action }
}

/// The composer's `/` commands, listed while the draft is just `/partial`, the same in a group: Fast mode runs here
/// through the host's `updateBot` (a group's on each member; a new session is in the bot's menu); a skill fills in `/<id> `, and the host turns that message into "read this skill first"
/// (`host/src/market/skills.rs` `expand_command`). docs/features/marketplace.md, "Commands".
enum SlashCommands {
    /// The command name being typed after a leading `/`, before any space.
    static func query(_ draft: String) -> String? {
        guard draft.hasPrefix("/"), !draft.contains(where: \.isWhitespace) else { return nil }
        return String(draft.dropFirst())
    }

    /// Built-in commands, then the skills the bot has on (a group's: any member's), whose name or
    /// title holds `query`. `members` are a group's bots.
    /// 按当前聊天列出适用命令，群成员共享会话切换语义。
    static func list(for bot: Bot?, members: [Bot] = [], query: String, skills: [InstalledSkill]) -> [SlashCommand] {
        guard let bot else { return [] }
        if query.hasPrefix("new") { return sessions(bot).filter { $0.name.contains(query) } }
        let owners = bot.isGroup ? members : [bot]
        let own = skills.filter { skill in owners.contains { $0.skills.contains(skill.id) } }.map {
            SlashCommand(name: $0.id, icon: "book.pages", title: $0.name, detail: $0.description, warning: false, action: .skill($0.id))
        }
        return (builtIn(bot, members: members) + own).filter {
            query.isEmpty || $0.name.localizedCaseInsensitiveContains(query) || $0.title.localizedCaseInsensitiveContains(query)
        }
    }

    /// A draft that is just a built-in command (`/fast`): sending it runs the command.
    /// 解析已确认的内置命令，选择候选不会自动发送消息。
    static func builtIn(for bot: Bot?, members: [Bot] = [], draft: String) -> SlashCommand? {
        guard let bot, let name = query(draft.trimmingCharacters(in: .whitespacesAndNewlines)) else { return nil }
        return (builtIn(bot, members: members) + sessions(bot)).first { $0.name == name }
    }

    /// /new 默认等待，另两项分别强制切换和撤回待切换标记。
    private static func sessions(_ bot: Bot) -> [SlashCommand] {
        guard !bot.isGroup else { return [] }
        return [("new", "after", "在当前任务结束后新建会话"), ("new-now", "immediate", "立即停止并新建"), ("new-cancel", "cancel", "取消切换")].map { name, mode, title in
            SlashCommand(name: name, icon: "plus", title: title, detail: "主会话与普通群共享；先保存当前执行状态", warning: false, action: .session(mode))
        }
    }

    /// Fast mode, while the bot (or a member of the group) shows it.
    private static func builtIn(_ bot: Bot, members: [Bot]) -> [SlashCommand] {
        let fast: (detail: String, state: Bot.FastModeState)
        if bot.isGroup {
            let group = GroupFastMode(members)
            guard !group.members.isEmpty else { return [] }
            fast = (group.detail, group.state)
        } else {
            guard bot.fastModeShown else { return [] }
            fast = (bot.fastModeDetail, bot.fastModeState)
        }
        return [SlashCommand(name: "fast", icon: "bolt", title: "Fast mode", detail: fast.detail,
                             warning: fast.state == .refused, action: .fast)]
    }
}

/// The `@partial` being typed at the end of a group's draft, if any. A member's full name and its space end it, so the
/// list doesn't stay up for a longer name that holds this one.
func mentionQuery(_ draft: String, members: [Bot]) -> String? {
    guard let at = draft.lastIndex(of: "@") else { return nil }
    if at > draft.startIndex, !draft[draft.index(before: at)].isWhitespace { return nil }
    let query = String(draft[draft.index(after: at)...])
    if query.first?.isWhitespace == true || query.contains(where: \.isNewline) || query.count > 24 { return nil }
    return members.contains { query.lowercased() == "\($0.name.lowercased()) " } ? nil : query
}
