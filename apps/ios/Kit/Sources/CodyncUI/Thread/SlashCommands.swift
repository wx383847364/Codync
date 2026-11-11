import CodyncKit
import Observation

/// A command in the composer's `/` list.
struct SlashCommand: Identifiable, Equatable {
    enum Action: Hashable { case fast, skill(String), session(String), file(WorkspaceFileCandidate) }

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

    /// 复用命令行布局，保留文件名和可区分同名文件的路径。
    static func workspaceFile(_ candidate: WorkspaceFileCandidate) -> SlashCommand {
        let detail = [candidate.source, candidate.file.path].filter { !$0.isEmpty }.joined(separator: " · ")
        return SlashCommand(name: candidate.file.path, icon: "doc", title: candidate.name, detail: detail, warning: false, action: .file(candidate))
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
            SlashCommand(name: name, icon: "plus", title: title, detail: "主会话与群共享；先保存当前执行状态", warning: false, action: .session(mode))
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

/// 持有当前斜杠查询的结果与高亮，网络调用经 BotStore 连接目标 Host。
@MainActor @Observable
final class WorkspaceFileSearch {
    struct Query: Hashable {
        let botId: String
        let text: String
        let end: Int
    }

    private var result: (query: Query, items: [WorkspaceFileCandidate], failed: Bool)?
    private var dismissed: Query?
    private(set) var index = 0

    /// 已关闭的查询在改变正文或位置后才会重新打开。
    func active(_ query: Query?) -> Bool { query != nil && query != dismissed }

    /// 只展示与当前查询对应且未关闭的候选。
    func items(for query: Query?) -> [SlashCommand] {
        guard active(query), result?.query == query else { return [] }
        return (result?.items ?? []).map(SlashCommands.workspaceFile)
    }

    /// 命令可继续使用，文件查询单独反馈等待、空结果和错误。
    func status(for query: Query?, hasCommands: Bool) -> String {
        guard active(query) else { return "" }
        guard let result, result.query == query else { return L10n.tr("Searching workspace files…") }
        if result.failed { return L10n.tr("Could not search workspace files.") }
        return hasCommands ? "" : L10n.tr("No matching workspace files")
    }

    /// 取消后的响应不能覆盖新光标处的候选。
    func refresh(_ query: Query?, store: BotStore) async {
        guard !Task.isCancelled else { return }
        result = nil
        dismissed = nil
        index = 0
        guard let query else { return }
        do {
            if !query.text.isEmpty { try await Task.sleep(for: .milliseconds(120)) }
            let items = try await store.searchWorkspaceFiles(botId: query.botId, query: query.text)
            try Task.checkCancellation()
            result = (query, items, false)
        } catch {
            if !Task.isCancelled { result = (query, [], true) }
        }
    }

    /// 关闭当前搜索，保留输入的斜杠词。
    func dismiss(_ query: Query?) { dismissed = query }

    /// 高亮首尾循环，并从仍可见的行开始。
    func move(by step: Int, count: Int) {
        guard count > 0 else { return }
        index = (min(index, count - 1) + step + count) % count
    }

    /// 结果在按键之间变化时，把高亮限制在当前列表内。
    func selected(in commands: [SlashCommand]) -> SlashCommand? {
        commands.isEmpty ? nil : commands[min(index, commands.count - 1)]
    }
}
