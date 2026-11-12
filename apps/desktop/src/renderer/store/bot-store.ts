import {
  isWorking,
  needsInput,
  normalizeBot,
  type Bot,
  type BotDraft,
  type DirListing,
  type Entry,
  type GroupDraft,
  type StageGroupDraft,
  type StageChange,
  type StageDetails,
  type StageUsage,
  type Hello,
  type ScreenState,
  type Usage,
} from '@shared/models'
import type { CallLine } from '@shared/calls'
import type { HostClient } from '../client/host-client'
import { latestOnly } from '../lib/latest-only'
import { updatePayload } from './bot-draft'
import { FileDownloads } from './file-downloads'
import { BotSections } from './bot-sections'
import { DevServerCommands } from './dev-servers'
import { sleep } from './bot-sync'
import { BotSending } from './bot-sending'

export { builtInBackendName, type Connection } from './bot-mirror'

export { type OutgoingFile, type SendOptions } from './bot-sending'

export const QUICK_REACTIONS = ['👍', '❤️', '😂', '🎉', '👀', '✅']

/**
 * One computer's mirror (bots, transcripts), kept by one events stream (catch-up since
 * `rev`, then live). Port of kit's `BotStore`.
 */
export class BotStore extends BotSending {
  readonly fileDownloads = new FileDownloads(() => this.changed())
  readonly sections = new BotSections(() => this.bots, () => this.changed(), (op, undo) => this.perform(op, true, undo))
  readonly devServers = new DevServerCommands((op) => this.perform(op), (op) => this.withLink(op))

  override retire() { this.fileDownloads.retire(); super.retire() }

  /** Asks this computer to update itself while it's behind this app (`installHostUpdate`). */
  requestHostUpdate() {
    this.hostUpdate = { kind: 'requested', at: Date.now() }
    this.changed()
    void this.withLink((c) => c.call('installHostUpdate', {})).catch((error: unknown) => {
      this.hostUpdate = { kind: 'failed', message: error instanceof Error ? error.message : String(error) }
      this.changed()
    })
  }



  stop(botId: string) {
    this.perform((c) => c.call('stop', { botId }), true)
  }

  /** Back to the bot's own agent from a fallback: now if it's idle, else from its next turn. The host posts the chat line. */
  switchBack(botId: string) {
    this.perform((c) => c.call('switchBack', { botId }), true)
  }

  /** A finished call's line, with the rest of its transcript; the chat then shows the call's messages behind it. */
  async endCall(botId: string, call: string, seconds: number, lines: CallLine[]) {
    try {
      // Safe to repeat: the computer ends a call once.
      const { entry } = await this.withLink((c) => c.call<{ entry: Entry | null }>('endCall', { botId, callId: call, seconds, lines }), true)
      if (entry) this.upsert(entry)
    } catch (error) {
      this.lastError = error instanceof Error ? error.message : String(error)
    }
  }

  /** A call's audio is live: the lead speaks first (only a host with leads marks one, so an older host never gets this). */
  callStarted(botId: string, call: string) {
    if (!this.bots.get(botId)?.lead) return
    // Its opening is the call's, like everything said on it.
    this.perform((c) => c.call('callStarted', { botId, callId: call }))
  }

  screenTakeover(on: boolean) {
    this.perform(async (c) => {
      this.screen = await c.call<ScreenState>('screenTakeover', { on })
      this.changed()
    }, true)
  }

  /** Only the computer itself may turn remote screen on. */
  async setScreenEnabled(on: boolean) {
    this.screen = await (await this.ready()).call<ScreenState>('setScreenEnabled', { enabled: on })
    this.changed()
  }

  /** Product analytics on this computer; undefined before hello. */
  get analytics() {
    return this.hello?.analytics
  }

  /** Only the computer itself may change analytics (the host accepts it from loopback). */
  async setAnalytics(enabled: boolean) {
    const res = await (await this.ready()).call<{ enabled: boolean }>('setAnalytics', { enabled })
    if (this.hello) this.hello = { ...this.hello, analytics: res.enabled }
    this.changed()
  }

  /** Toggles the user's reaction; shown at once, then replaced by the host's copy. */
  react(entry: Entry, emoji: string) {
    const list = this.entries.get(entry.botId)
    const i = list?.findIndex((e) => e.id === entry.id) ?? -1
    if (!list || i < 0) return
    const reactions = [...(list[i]!.data.reactions ?? [])]
    const at = reactions.indexOf(emoji)
    if (at >= 0) reactions.splice(at, 1)
    else reactions.push(emoji)
    const next = [...list]
    next[i] = { ...next[i]!, data: { ...next[i]!.data, reactions } }
    this.entries.set(entry.botId, next)
    this.changed()
    this.perform(async (c) => {
      const res = await c.call<{ entry: Entry }>('react', { entryId: entry.id, emoji })
      this.upsert(res.entry)
      this.changed()
    })
  }

  /** A card's button: `entry` shows `choice` as answering (its spinner) while `op` runs. */
  private answer(entry: Entry, choice: string, op: (c: HostClient) => Promise<unknown>) {
    if (this.answering.has(entry.id)) return
    this.answering.set(entry.id, choice)
    this.changed()
    void (async () => {
      try {
        await this.withLink(op, true)
      } catch (error) {
        this.lastError = error instanceof Error ? error.message : String(error)
      }
      this.answering.delete(entry.id)
      this.changed()
    })()
  }

  respond(entry: Entry, option: string | null) {
    this.answer(entry, option ?? '', async (c) => {
      await c.call('respondPermission', { entryId: entry.id, optionId: option })
      // The card's own update follows on the events stream; hold the spinner until then.
      await sleep(2000)
    })
  }

  /** Has a queued message handled now (`steer`): the computer's update takes it out of the queue. */
  steer(entry: Entry) {
    this.answer(entry, 'steer', (c) => c.call('steer', { botId: entry.botId, entryId: entry.id }))
  }

  /** Takes back a queued message (`unqueue`): gone at once, back if the computer refuses. */
  unqueue(entry: Entry) {
    const show = (e: Entry) => {
      this.upsert(e)
      this.changed()
    }
    show({ ...entry, data: { ...entry.data, status: 'cancelled', removed: true } })
    this.perform((c) => c.call('unqueue', { botId: entry.botId, entryId: entry.id }), true, () => show(entry))
  }

  /** Saves the user's version of a handoff that hasn't been sent; whether it did. */
  async editHandoff(entry: Entry, text: string) {
    try {
      await this.withLink((c) => c.call('editHandoff', { entryId: entry.id, text }), true)
      return true
    } catch (error) {
      this.lastError = error instanceof Error ? error.message : String(error)
      this.changed()
      return false
    }
  }

  /** Carries on with the bot's current conversation instead of the handoff. */
  keepConversation(entry: Entry) {
    this.answer(entry, 'handoff', (c) => c.call('keepConversation', { entryId: entry.id }))
  }

  /** Runs a failed routine run's work again, from its notice; the notice's update drops the button. */
  retryRoutineRun(entry: Entry) {
    const runId = entry.data.runId
    if (runId) this.answer(entry, 'retry', (c) => c.call('retryRoutineRun', { botId: entry.botId, runId }))
  }

  /** The roster's "Mark as read". */
  markAllRead(botId: string) {
    const bot = this.bots.get(botId)
    if (!bot || bot.unread <= 0) return
    this.bots.set(botId, { ...bot, unread: 0 })
    this.changed()
    this.perform((c) => c.call('markRead', { botId }), true)
  }

  private patchBot(bot: Bot, change: Partial<BotDraft>) {
    const current = this.bots.get(bot.id)
    if (current) this.bots.set(bot.id, { ...current, ...change } as Bot)
    this.changed()
    this.perform((c) => c.call('updateBot', { ...updatePayload(bot), ...change }), true)
  }

  setPinned(bot: Bot, pinned: boolean) {
    this.patchBot(bot, { pinned })
  }

  /** Whether the bot may see and use the computer's screen. */
  setComputer(bot: Bot, computer: boolean) {
    this.patchBot(bot, { computer })
  }

  /** Whether the bot uses the memory all bots share. */
  setSharedMemory(bot: Bot, sharedMemory: boolean) {
    this.patchBot(bot, { sharedMemory })
  }

  /** Whether the bot's harness keeps its own memory on beside Codync's. */
  setHarnessMemory(bot: Bot, harnessMemory: boolean) {
    this.patchBot(bot, { harnessMemory })
  }

  /** Makes `bot` the lead, in place of the one before; a refusal gives the star back. */
  setLead(bot: Bot) {
    const before = [...this.bots.values()].find((b) => b.lead)?.id
    const show = (lead: string | undefined) => {
      for (const [id, b] of this.bots) this.bots.set(id, { ...b, lead: id === lead })
      this.changed()
    }
    show(bot.id)
    this.perform((c) => c.call('setLead', { botId: bot.id }), true, () => show(before))
  }

  setHidden(bot: Bot, hidden: boolean) {
    this.patchBot(bot, { hidden })
  }

  setFastMode(bot: Bot, fastMode: boolean) {
    this.patchBot(bot, { fastMode })
  }

  /** The roster as the user dragged it, top first: shown at once, then saved on the host. */
  reorder(ids: string[]) {
    ids.forEach((id, position) => {
      const bot = this.bots.get(id)
      if (bot) this.bots.set(id, { ...bot, position })
    })
    this.changed()
    this.perform((c) => c.call('reorderBots', { ids }), true)
  }

  delete(bot: Bot) {
    this.composerDrafts.removeBot(bot.id)
    this.bots.delete(bot.id)
    this.entries.delete(bot.id)
    this.onRosterChanged?.()
    this.changed()
    this.perform((c) => c.call('deleteBot', { botId: bot.id }), true)
  }

  /** Creates a group chat (or opens the one these bots already share) and selects it. */
  async createGroup(name: string, description: string, members: string[]) {
    const draft: GroupDraft = { kind: 'group', name, description, members }
    const res = await (await this.ready()).call<{ bot: Bot }>('createBot', draft)
    const bot = this.putHostBot(normalizeBot(res.bot))
    this.selection = bot.id
    this.onRosterChanged?.()
    this.changed()
    return bot
  }

  /** 复制群设置并通过新群的 ID 打开空白会话，失败沿用现有错误提示。 */
  createGroupSession(botId: string) {
    // 新群的创建不能自动重放，避免断线时提交成功却又多建一个。
    return this.perform(async (client) => {
      const res = await client.call<{ bot: Bot }>('createGroupSession', { botId })
      this.selection = this.putHostBot(normalizeBot(res.bot)).id
      this.onRosterChanged?.()
      this.changed()
    })
  }

  /** 接收已提交的新群，将首条消息存入本地草稿后打开。 */
  async createStageGroup(draft: StageGroupDraft) {
    const res = await (await this.ready()).call<{ bot: Bot; draft: string }>('createStageGroup', draft)
    const bot = this.putHostBot(normalizeBot(res.bot))
    if (!bot.lastMessage && !this.composerDrafts.get(bot.id)) this.composerDrafts.set(bot.id, res.draft)
    this.selection = bot.id
    this.onRosterChanged?.()
    this.changed()
  }

  /** 详情和人工阶段变更使用同一 Host 投影，保留当前聊天位置。 */
  async stageDetails(botId: string, change?: StageChange): Promise<StageDetails> {
    const details = await (await this.ready()).call<StageDetails>(change ? 'changeStage' : 'stageDetails', { botId, ...change })
    this.putHostBot(normalizeBot(details.bot))
    this.changed()
    return details
  }

  /** 阶段用量独立读取，界面故障不会影响交接或关闭。 */
  async stageUsage(botId: string): Promise<StageUsage> {
    return (await this.ready()).call<StageUsage>('stageUsage', { botId })
  }

  async updateGroup(draft: GroupDraft) {
    const res = await (await this.ready()).call<{ bot: Bot }>('updateBot', draft)
    this.putHostBot(normalizeBot(res.bot))
    this.changed()
  }

  /** Takes one bot out of a group; an automatic "Alice, Bob" name follows the new roster. */
  removeMember(group: Bot, memberId: string) {
    const members = group.members.filter((m) => m !== memberId)
    const autoName = (ids: string[]) => ids.map((id) => this.bots.get(id)?.name).join(', ')
    const name = group.name === autoName(group.members) ? autoName(members) : group.name
    this.bots.set(group.id, { ...group, name, members })
    this.changed()
    const draft: GroupDraft = { id: group.id, kind: 'group', name, description: group.description, members, pinned: group.pinned }
    this.perform((c) => c.call('updateBot', draft), true)
  }

  /** What a voice call carried, from the computer: the loaded chat may not reach back to it. */
  async callEntries(botId: string, call: string): Promise<Entry[]> {
    return (await (await this.ready()).call<{ entries: Entry[] }>('callEntries', { botId, callId: call })).entries
  }

  /** The history between a bot and one peer. Never upserted: old notices would break `loadOlder`'s paging. */
  async botConversation(botId: string, peerId: string): Promise<Entry[]> {
    return (await this.ready()).botConversation(botId, peerId)
  }

  /** What `backend` already has in the user's home, not the Codync skills and connectors a bot passes on. */
  async agentGear(backend: string): Promise<{ skills: AgentGearItem[]; connectors: AgentGearItem[]; rules: AgentGearItem[]; plugins: AgentGearItem[] }> {
    const client = this.client
    if (!client) throw new Error("Couldn't load what this agent brings.")
    const value = await client.call<{ skills?: AgentGearItem[]; connectors?: AgentGearItem[]; rules?: AgentGearItem[]; plugins?: AgentGearItem[] }>('agentGear', { backend })
    return { skills: value.skills ?? [], connectors: value.connectors ?? [], rules: value.rules ?? [], plugins: value.plugins ?? [] }
  }

  async save(draft: BotDraft) {
    const client = await this.ready()
    const res = draft.id
      ? await client.call<{ bot: Bot }>('updateBot', { ...draft, model: draft.model ?? null })
      : await client.call<{ bot: Bot }>('createBot', draft)
    const bot = this.putHostBot(normalizeBot(res.bot))
    this.onRosterChanged?.()
    this.changed()
    return bot
  }

  async refreshBackends() {
    if (!this.client || !this.hello) return
    try {
      const res = await this.client.call<{ backends: Hello['backends'] }>('refreshBackends', {}, 60_000)
      this.hello = { ...this.hello, backends: res.backends }
      this.changed()
    } catch {}
  }

  async selectWorkspace(path: string) {
    return (await this.ready()).call<{ path: string }>('selectWorkspace', { path })
  }

  async listDirs(path: string | null) {
    return (await this.ready()).call<DirListing>('listDirs', { path })
  }

  /** Whether this computer lists one agent's limits. */
  async setUsageShown(id: string, shown: boolean) {
    this.usage = await (await this.ready()).call<Usage>('setUsageShown', { id, shown })
    this.changed()
  }

  /** The order this computer lists its agents in. */
  setUsageOrder = latestOnly(async (ids: string[]) => {
    this.usage = await (await this.ready()).call<Usage>('setUsageOrder', { ids })
    this.changed()
  })

  async refreshUsage() {
    if (this.retired || !this.client) return
    try {
      this.usage = await this.client.call<Usage>('usage', { refresh: true })
      this.changed()
    } catch {}
  }

  async loadOlder(botId: string) {
    if (!this.client || this.historyComplete.has(botId)) return
    const first = this.allEntries(botId).find((e) => e.seq > 0 && e.seq !== Number.MAX_SAFE_INTEGER)?.seq ?? Number.MAX_SAFE_INTEGER
    try {
      const older = await this.client.history(botId, first, 100)
      if (older.length < 100) this.historyComplete.add(botId)
      for (const e of older) this.upsert(e)
      this.changed()
    } catch {}
  }

  setSelection(id: string | null) {
    this.selection = id
    this.changed()
  }
}

export { isWorking, needsInput }

/** One skill or connector an agent keeps for itself (`agentGear`). */
export interface AgentGearItem {
  name: string
  description?: string
  detail?: string
}
