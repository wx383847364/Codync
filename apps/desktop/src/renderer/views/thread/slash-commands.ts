// The composer's `/` commands, listed while the draft is just `/partial`: Fast mode runs here
// through the host's `updateBot` (a new session is in the bot's menu); a skill fills in `/<id> `, and the host turns that message into "read this skill first"
// (host/src/market/skills.rs `expand_command`). A group lists its members' commands: Fast mode
// for all of them, and every skill one of them has. docs/features/marketplace.md, "Commands".
import { fastModeDetail, fastModeShown, fastModeState, groupFastMode, type FastModeState } from '../../../shared/fast-mode.ts'
import type { WorkspaceFileCandidate } from '../../../shared/mentions.ts'
import type { Bot } from '../../../shared/models.ts'

export type SlashAction = { kind: 'fast' } | { kind: 'session'; mode: string } | { kind: 'skill'; id: string } | { kind: 'file'; candidate: WorkspaceFileCandidate }

export interface SlashCommand {
  /** What follows the `/`. */
  name: string
  icon: string
  title: string
  detail: string
  warning: boolean
  action: SlashAction
}

/** The command name being typed after a leading `/`, before any space; otherwise null. */
export function slashQuery(draft: string): string | null {
  return /^\/\S*$/.test(draft) ? draft.slice(1) : null
}

/** 光标前的斜杠词可以是路径，普通 URL 和词内斜杠不打开搜索。 */
export function workspaceFileQuery(draft: string, start: number, end: number) {
  if (start !== end || start <= 0 || start > draft.length || /\s/u.test(draft[start - 1]!)) return null
  if (start < draft.length && !/\s/u.test(draft[start]!)) return null
  const token = draft.slice(0, start).match(/(?:^|\s)(\/[^\s]*)$/u)?.[1]
  return token && !token.startsWith('//') && token.length <= 256
    ? { query: token.slice(1), start: start - token.length, end: start }
    : null
}

/** 文件候选沿用命令列表，显示原始文件名及来源目录。 */
export function workspaceFileCommand(candidate: WorkspaceFileCandidate): SlashCommand {
  return {
    name: `${candidate.file.workspace}/${candidate.file.path}`,
    icon: 'doc',
    title: candidate.name,
    detail: [candidate.source, candidate.file.path].filter(Boolean).join(' · '),
    warning: false,
    action: { kind: 'file', candidate },
  }
}

/** The bot's built-in commands; a group's Fast mode is its members' (`groupFastMode`). */
function builtIn(bot: Bot, members: Bot[]): SlashCommand[] {
  const fast = bot.kind === 'group' ? groupFastMode(members) : ownFastMode(bot)
  return fast ? [{ name: 'fast', icon: 'bolt', title: 'Fast mode', detail: fast.detail, warning: fast.state === 'refused', action: { kind: 'fast' } }] : []
}

/** /new 提供三项明确选择，默认等待；主会话和所有群共享新会话。 */
function sessions(bot: Bot): SlashCommand[] {
  if (bot.kind === 'group') return []
  return [
    ['new', 'after', '在当前任务结束后新建会话'],
    ['new-now', 'immediate', '立即停止并新建'],
    ['new-cancel', 'cancel', '取消切换'],
  ].map(([name, mode, title]) => ({ name: name!, icon: 'plus', title: title!, detail: '主会话与群共享；先保存当前执行状态', warning: false, action: { kind: 'session', mode: mode! } }))
}

/** A bot's own Fast mode, while it shows it. */
function ownFastMode(bot: Bot): { state: FastModeState; detail: string } | null {
  return fastModeShown(bot) ? { state: fastModeState(bot), detail: fastModeDetail(bot) } : null
}

/**
 * Built-in commands, then the bot's skills (ids in `bot.skills`; a group's `members` have them),
 * whose name or title holds `query`.
 */
/** 按输入前缀列出群成员与会话命令，避免无关选项挤占候选。 */
export function slashCommands(bot: Bot | null, query: string, installedSkills: { id: string; name: string; description: string }[], members: Bot[] = []): SlashCommand[] {
  if (!bot) return []
  const enabled = bot.kind === 'group' ? members.flatMap((m) => m.skills) : bot.skills
  const skills = installedSkills
    .filter((s) => enabled.includes(s.id))
    .map((s): SlashCommand => ({ name: s.id, icon: 'book.pages', title: s.name, detail: s.description, warning: false, action: { kind: 'skill', id: s.id } }))
  if (query.startsWith('new')) return sessions(bot).filter((c) => c.name.includes(query))
  const q = query.toLowerCase()
  return [...builtIn(bot, members), ...skills].filter((c) => c.name.includes(q) || c.title.toLowerCase().includes(q))
}

/** A draft that is just a built-in command (`/fast`): sending it runs the command. */
/** 解析已确认的内置命令，选人只插入正文而不执行发送。 */
export function builtInCommand(bot: Bot | null, draft: string, members: Bot[] = []): SlashCommand | null {
  const name = slashQuery(draft.trim())
  return bot && name !== null ? ([...builtIn(bot, members), ...sessions(bot)].find((c) => c.name === name) ?? null) : null
}

/**
 * What Return does with `picked` highlighted: run it, except a skill whose name is already
 * typed out, which sends.
 */
export function returnRuns(picked: SlashCommand, draft: string): boolean {
  return picked.action.kind !== 'skill' || draft !== `/${picked.name}`
}
