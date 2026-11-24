import { useContext, useEffect, useId, useState } from 'react'
import { hasLibrary } from '@shared/artifacts'
import { isGroup, isWorking, isWorkingIn, type Bot, type BotSessionUsage } from '@shared/models'
import { AgentIcon } from '../../components/AgentIcon'
import { BotAvatar, GroupAvatar } from '../../components/Avatar'
import { Button, CardSection, Disclosure, IconButton, Reveal, RowLabel, Switch, TabChoice } from '../../components/Controls'
import { LeadAgentBadge } from '../../components/LeadAgentBadge'
import { Icon } from '../../components/Icon'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { AppContext, useStore, useStoreRef } from '../../store/context'
import { analytics, useScreenViewed } from '../../store/analytics'
import { BotSettingsPanel } from '../bots/BotEditorView'
import { MemoryCard } from '../bots/MemoryView'
import { RoutinesView } from '../routines/RoutinesView'
import { LibraryView } from '../library/LibraryView'
import { DevServersSection } from './DevServersSection'
import { GroupContextAction, GroupContextHeading, GroupContextReading, GroupContextReview, useGroupContexts } from './GroupContexts'
import { SessionHistory } from './SessionHistory'
import { MacChatButton } from './MacChatButton'
import '../bots/bots.css'
import './details-panel.css'
import { useTr } from '../../lib/use-language'


export type DetailsTab = 'details' | 'library' | 'memory' | 'settings'

const TABS: { id: DetailsTab; label: string }[] = [
  { id: 'details', label: 'Details' },
  { id: 'library', label: 'Library' },
  { id: 'memory', label: 'Memory' },
  { id: 'settings', label: 'Settings' },
]

/** The inspector beside a conversation (Grok Bot's): who the bot is, then its details, library, memory and settings. */
export function DetailsPanel({ botId, routineId, tab, setTab, editGroup, openArtifact, share, close }: {
  botId: string
  routineId: string | null
  tab: DetailsTab
  setTab: (tab: DetailsTab) => void
  editGroup: () => void
  openArtifact: (artifactId: string) => void
  share: () => void
  close: () => void
}) {
  const store = useStore()
  const bot = store.bots.get(botId) ?? null
  const group = !!bot && isGroup(bot)
  const contexts = useGroupContexts()
  const reviewing = group && !!contexts?.review
  useScreenViewed('bot_details')
  return (
    <div className="details-panel">
      <div className="details-drag" />
      <div className="details-actions">
        {bot && !group ? <MacChatButton className="ringed" title="Create template" icon="square.and.arrow.up" onClick={share} /> : null}
        <MacChatButton className="ringed" title="Close details" icon="xmark" onClick={close} />
      </div>
      <div className="details-scroll">
        {bot && !reviewing ? <Identity bot={bot} /> : null}
        {bot && group ? (
          reviewing ? <GroupContextReview /> : <><GroupMembersList group={bot} editGroup={editGroup} />
            {store.hello?.groupResourceUsage && <GroupResourceUsage key={bot.id} group={bot} />}</>
        ) : (
          <>
            <TabChoice label="Conversation details" selection={tab} options={hasLibrary(store.hostVersion?.version) ? TABS : TABS.filter((t) => t.id !== 'library')} onChange={setTab} />
            <div key={tab} className="details-tab fade-in">
              {tab === 'details' ? (
                <>
                  {bot && store.hello?.botWorkspaceUsage ? <BotWorkspaceUsage key={bot.id} botId={bot.id} /> : null}
                  {bot ? <DevServersSection bot={bot} /> : null}
                  <BotSettingsPanel botId={botId} part="often" />
                  {bot ? <ComputerUse bot={bot} /> : null}
                  {bot && store.hello?.botSessionUsage ? <SessionUsage key={bot.id} bot={bot} close={close} /> : null}
                  <RoutinesView botId={botId} initialId={routineId} />
                </>
              ) : tab === 'library' ? (
                <LibraryView botId={botId} openArtifact={openArtifact} />
              ) : tab === 'memory' ? (
                <MemoryCard botId={botId} />
              ) : (
                <BotSettingsPanel botId={botId} part="rarely" />
              )}
            </div>
          </>
        )}
      </div>
    </div>
  )
}

/** 打开本机 Host 返回的工作区，并上报操作结果。 */
async function openWorkspace(store: ReturnType<typeof useStore>, directory: string, tr: (key: string) => string) {
  const outcome = await window.codync.fileLinks.act(store.computer.id, directory, 'open').catch(() => 'failed' as const)
  analytics.track({ event: 'file_link_action', action: 'open', outcome })
  if (outcome !== 'ok') store.setError(tr('Could not open the workspace folder.'))
}

/** 显示 Host 托管工作区大小，并在运行 Host 的桌面打开目录。 */
function BotWorkspaceUsage({ botId }: { botId: string }) {
  const store = useStore()
  const app = useContext(AppContext)
  const storeRef = useStoreRef()
  const tr = useTr()
  const online = !store.isOffline && !store.mismatch
  const [usage, setUsage] = useState<{ bytes: number | null; directory?: string } | null>(null)

  // 详情重新打开或连接恢复时读取工作区，忽略已卸载页面的响应。
  useEffect(() => {
    if (!online) return
    let current = true
    void store.ready().then(client => client.call<{ bytes: number | null; directory?: string }>('botWorkspaceUsage', { botId })).then(
      result => { if (current) setUsage(result) },
      () => { if (current) setUsage({ bytes: null }) },
    )
    return () => { current = false }
  }, [store, botId, online])

  const directory = online && app?.local === storeRef && store.isLoopback ? usage?.directory : undefined
  return <div className="details-group-usage-card">
    <HStack gap={10}>
      <span className="details-usage-glyph"><Icon name="folder" size={16} /></span>
      <VStack grow>
        <Text weight="semibold">{tr('Disk usage')}</Text>
        <Text size="footnote" tone="secondary">{tr('Managed workspace')}</Text>
      </VStack>
      <strong>{!online || usage?.bytes === null ? tr('Unavailable') : usage ? bytes(usage.bytes) : tr('Loading…')}</strong>
      {directory && <IconButton icon="folder" title={tr('Open folder')} onClick={() => {
        // 沿用群工作区的本机文件操作。
        void openWorkspace(store, directory, tr)
      }} />}
    </HStack>
  </div>
}

/** 展示主会话实报资源以及最近的任务尝试，保留执行器各字段的口径。 */
function SessionUsage({ bot, close }: { bot: Bot; close: () => void }) {
  const store = useStore()
  const tr = useTr()
  const [usage, setUsage] = useState<BotSessionUsage | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [refresh, setRefresh] = useState(0)
  const [preparing, setPreparing] = useState(false)
  const [expanded, setExpanded] = useState<string | null>(null)
  const online = !store.isOffline

  // 展开详情、手动刷新或任务状态切换时读取最新快照，忽略过时响应。
  useEffect(() => {
    if (!online) return
    let active = true
    store.botSessionUsage(bot.id).then(
      (result) => { if (active) { setUsage(result); setError(null) } },
      (failure: unknown) => { if (active) setError(String(failure instanceof Error ? failure.message : failure)) },
    )
    return () => { active = false }
  }, [store, bot.id, bot.status, bot.mainSessionRevision, online, refresh])

  /** 用户主动准备交接后回到聊天，方便查看摘要。 */
  const prepare = async () => {
    setPreparing(true)
    setError(null)
    try {
      await store.prepareHandoff(bot.id)
      close()
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure))
      setPreparing(false)
    }
  }

  /** 未上报与已知的零分别展示。 */
  const number = (value: number | null) => value?.toLocaleString() ?? tr('Unavailable')
  const context = usage ? usage.hasSession ? `${number(usage.used)} / ${number(usage.size)}` : tr('No active main session.')
    : tr(online ? 'Loading…' : 'Unavailable')
  return (
    <VStack gap={10}>
      <CardSection title={tr('Main session context')} accessory={
        <IconButton title="Refresh usage" icon="arrow.clockwise" disabled={!online} onClick={() => setRefresh(refresh + 1)} />
      }>
        <RowLabel label={context} />
      </CardSection>
      {store.hello?.sessionHistory && <SessionHistory botId={bot.id} />}
      {error ? <Text size="footnote">{tr(error)}</Text> : null}
      {usage?.hasSession ? <Button kind="secondary" disabled={!online || isWorking(bot) || preparing} onClick={() => { void prepare() }}>
        {tr('Prepare new session with summary')}
      </Button> : null}
      {usage ? <CardSection title={tr('Recent task attempts')}>
        {usage.attempts.length === 0 ? <RowLabel label={tr('No recent task attempts')} /> : usage.attempts.map((attempt) => {
          const key = attempt.id
          const date = new Date(attempt.at).toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'short' })
          return (
            <Disclosure key={key} expanded={expanded === key} onToggle={() => setExpanded(expanded === key ? null : key)}
              label={<Text size="footnote">{date} · {attempt.backend} · {attempt.model ?? tr('Unknown model')}</Text>}>
              <VStack gap={4}>
                {attempt.rows.map((row) => <Text size="footnote" key={row.label}>{tr(row.label)} · {number(row.value)}</Text>)}
              </VStack>
            </Disclosure>
          )
        })}
      </CardSection> : null}
    </VStack>
  )
}

/** The bot's avatar and name, and what runs it (a group: how many bots it has). */
function Identity({ bot }: { bot: Bot }) {
  const store = useStore()
  const group = isGroup(bot)
  return (
    <div className="details-identity">
      {group ? <GroupAvatar members={store.members(bot)} size={48} /> : <div className="avatar-frame"><BotAvatar bot={bot} size={48} />{bot.lead ? <LeadAgentBadge size={24} interactive /> : null}</div>}
      <span className="details-name">{bot.name}</span>
      <span className="details-subtitle">
        {group ? (
          `${bot.members.length} ${bot.members.length === 1 ? 'bot' : 'bots'}`
        ) : (
          <>
            <AgentIcon registry={store.agentRegistry(bot.backend)} size={12} />
            {store.backendName(bot.backend)}
            {bot.model ? ` · ${bot.model}` : null}
          </>
        )}
      </span>
    </div>
  )
}

/** Whether this bot may use the computer, switched right here (also in Settings). */
function ComputerUse({ bot }: { bot: Bot }) {
  const tr = useTr()
  const store = useStore()
  const screen = store.screen
  if (!screen) return null
  const ready = screen.platform === 'windows' || (screen.enabled && screen.connected && screen.capture && screen.input)
  return (
    <section className="details-section">
      <Text className="details-section-title">{tr(`Computer`)}</Text>
      <div className="outlined-card details-switch">
        <RowLabel grow label="Use the computer" detail={ready ? null : 'On that computer, open Codync → Settings → Computer access to set it up first.'} />
        <HStack as="span" align="stretch" className="details-switch-control">
          <Switch label="Use the computer" on={bot.computer} disabled={store.isOffline} onChange={(on) => store.setComputer(bot, on)} />
        </HStack>
      </div>
    </section>
  )
}

/** 成员详情沿用原头像、私聊和移除入口，同一行显示用量和新开会话。 */
function GroupMembersList({ group, editGroup }: { group: Bot; editGroup: () => void }) {
  const tr = useTr()
  const store = useStore()
  const contexts = useGroupContexts()
  return (
    <section className="details-members" aria-label={tr(contexts ? 'Member context' : 'Members')}>
      <GroupContextHeading />
      {/* 旧 Host 继续提供成员管理，支持用量的 Host 为每个成员加一行读数。 */}
      {store.members(group).map((bot) => (
        <div key={bot.id} className={contexts ? 'group-context-card' : undefined}>
          <div className="member-row removable">
            <button className="member-open press" title={tr("Open {name}'s own chat").replaceAll('{name}', () => bot.name)}
              onClick={() => { /* 点击头像或名字仍打开私聊。 */ store.setSelection(bot.id) }}>
              <BotAvatar bot={bot} size={32} />
              <VStack as="span" gap={1} grow>
                <span className="member-name">{bot.name}</span>
                {!store.hello?.groupMemberActivity && isWorkingIn(bot, group.id) ? <Text size="compactSecondary" tone="secondary" className="member-name">{tr(bot.activity || 'Working…')}</Text> : null}
              </VStack>
            </button>
            <GroupContextAction botId={bot.id} />
            {group.members.length > 1 ? <IconButton className="member-remove"
              title={tr('Remove {name} from the group').replaceAll('{name}', () => bot.name)} icon="xmark"
              onClick={() => { /* 移除入口与会话操作各自独立。 */ store.removeMember(group, bot.id) }} /> : null}
          </div>
          {contexts?.snapshot?.members.some(row => row.botId === bot.id) && store.hello?.groupMemberActivity
            && isWorkingIn(bot, group.id) && <Button kind="secondary" title={tr('Member activity')} onClick={() => {
              // 只显示本群执行状态，点击打开本群任务。
              contexts.inspect(bot.id)
            }}>{tr(bot.activity || 'Working…')}</Button>}
          <GroupContextReading bot={bot} />
        </div>
      ))}
      {contexts?.error ? <Text size="caption" tone="secondary" role="status">{contexts.error}</Text> : null}
      <button className="member-row press" onClick={editGroup}>
        <span className="member-add-icon">
          <Icon name="plus" size={16} color="var(--secondary)" />
        </span>
        <Text tone="secondary" className="member-name">{tr('Add Member')}</Text>
      </button>
    </section>
  )
}

interface GroupUsage {
  resourceBytes: number | null
  resourceDirectory?: string | null
  groupTokens: number | null
  hostTokens: number | null
  share: number | null
  partial: boolean
}

/** 在群详情中展示已归属资源和实报用量，本机目录交由既有文件入口打开。 */
function GroupResourceUsage({ group }: { group: Bot }) {
  const store = useStore()
  const app = useContext(AppContext)
  const storeRef = useStoreRef()
  const visible = useGroupContexts()?.panelVisible ?? true
  const tr = useTr()
  const [usage, setUsage] = useState<GroupUsage | null>(null)
  const [failed, setFailed] = useState(false)
  const [opening, setOpening] = useState(false)
  const online = !store.isOffline && !store.mismatch
  const stamp = [group.rev, ...store.members(group).map(bot => bot.status)].join('|')

  useEffect(() => {
    if (!online || !visible) return
    let active = true
    // 合并同一回合的群事件，收尾后刷新实报与文件占用。
    const timer = setTimeout(async () => {
      try {
        const value = await (await store.ready()).call<GroupUsage>('groupResourceUsage', { roomId: group.id })
        if (active) { setUsage(value); setFailed(false) }
      } catch {
        if (active) setFailed(true)
      }
    }, 150)
    return () => { active = false; clearTimeout(timer) }
  }, [store, group.id, stamp, online, visible])

  const directory = app?.local === storeRef && store.isLoopback ? usage?.resourceDirectory : null
  /** 点击后只打开本机 Host 给出的群目录，并复用匿名文件操作事件。 */
  async function openDirectory() {
    if (!directory || opening) return
    setOpening(true)
    const outcome = await window.codync.fileLinks.act(store.computer.id, directory, 'open').catch(() => 'failed' as const)
    analytics.track({ event: 'file_link_action', action: 'open', outcome })
    if (outcome !== 'ok') store.setError(tr('Could not open the group resource folder.'))
    setOpening(false)
  }

  const unavailable = tr(!online || failed ? 'Unavailable' : 'Loading…')
  const share = usage?.share == null ? '—' : usage.share.toLocaleString(undefined, { style: 'percent', maximumFractionDigits: 1 })
  const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 })
  return <section className="details-group-usage" aria-label={tr('Resources and usage')}>
    <Text weight="semibold">{tr('Resources and usage')}</Text>
    <div className="details-group-usage-card">
      <div className="details-group-usage-row">
        <span className="details-usage-glyph"><Icon name="folder" size={16} /></span>
        <div>
          <div>{tr('Group attachments and shared files')}</div>
          <strong title={usage?.resourceBytes?.toLocaleString()}>{usage?.resourceBytes == null ? usage ? tr('Unavailable') : unavailable : bytes(usage.resourceBytes)}</strong>
        </div>
        {directory && <Button kind="secondary" disabled={!online || opening} className="details-usage-action" onClick={() => void openDirectory()}>
          {tr(window.codync.platform === 'darwin' ? 'Open in Finder' : 'Open folder')} <Icon name="arrow.up.right" size={12} />
        </Button>}
      </div>
    </div>
    {store.hello?.groupWorkspaceUsage && <GroupWorkspaceUsage group={group} />}
    <div className="details-group-usage-card">
      <div className="details-group-usage-row">
        <span className="details-usage-glyph usage"><Icon name="chart.bar" size={16} /></span>
        <div>
          <div>{tr('Group token usage')}</div>
          <strong title={usage?.groupTokens?.toLocaleString()}>{usage?.groupTokens == null ? usage ? tr('No usage recorded') : unavailable : `${compact.format(usage.groupTokens)} Token`}</strong>
        </div>
        {usage?.share != null && <div className="details-usage-share"><strong>{share}</strong><div>{tr('Share of host usage')}</div></div>}
      </div>
      <div className="details-usage-foot">
        {usage?.share != null && <div className="details-usage-progress" role="progressbar" aria-label={tr('Share of host usage')} aria-valuenow={usage.share * 100}>
          <span style={{ width: `${usage.share * 100}%` }} />
        </div>}
        {usage?.hostTokens != null && <div title={usage.hostTokens.toLocaleString()}>{tr('All conversations on this host: {tokens} Token').replace('{tokens}', compact.format(usage.hostTokens))}</div>}
        {usage?.partial && <div>{tr('Partial usage records')}</div>}
        {usage && (!online || failed) && <div role="status">{tr('Could not refresh usage.')}</div>}
      </div>
    </div>
  </section>
}


type WorkspaceUsage = {
  totalBytes: number | null
  members: { botId: string; name: string; bytes: number | null; shared: boolean; directory?: string }[]
}

/** 用十进制存储单位，且完整值仍可从数值的提示文字查看。 */
function bytes(value: number) {
  const unit = Math.min(4, Math.floor(Math.log10(Math.max(1, value)) / 3))
  return `${(value / 1000 ** unit).toLocaleString(undefined, { maximumFractionDigits: 2 })} ${['B', 'KB', 'MB', 'GB', 'TB'][unit]}`
}

/** 工作区用独立请求，只在资料页可见或成员变更时读取 Host 的缓存。 */
function GroupWorkspaceUsage({ group }: { group: Bot }) {
  const store = useStore()
  const app = useContext(AppContext)
  const storeRef = useStoreRef()
  const local = app?.local === storeRef && store.isLoopback
  const visible = useGroupContexts()?.panelVisible ?? true
  const tr = useTr()
  const online = !store.isOffline && !store.mismatch
  const listId = useId()
  const membership = group.members.join('|')
  const [usage, setUsage] = useState<WorkspaceUsage | null>(null)
  const [failed, setFailed] = useState(false)
  const [expanded, setExpanded] = useState(false)

  useEffect(() => {
    // 名单变更时先撤下旧行，避免失败后继续提供已移除成员的目录。
    setUsage(null)
    setFailed(false)
    if (!online || !visible) return
    let current = true
    // 群切换或成员名单变化时，忽略上一次请求迟到的结果。
    void store.ready().then(c => c.call<WorkspaceUsage>('groupWorkspaceUsage', { roomId: group.id })).then(
      (result) => { if (current) { setUsage(result); setFailed(false) } },
      () => { if (current) setFailed(true) },
    )
    return () => { current = false }
  }, [store, group.id, membership, online, visible])

  const total = usage?.totalBytes == null
    ? tr(usage || failed || !online ? 'Unavailable' : 'Loading…')
    : bytes(usage.totalBytes)
  return <div className="details-group-usage-card details-workspaces">
    <button type="button" className="details-workspace-heading" aria-expanded={expanded} aria-controls={expanded && usage?.members.length ? listId : undefined} onClick={() => setExpanded(!expanded)}>
      <span className="details-usage-glyph"><Icon name="folder" size={16} /></span>
      <span className="details-workspace-label">
        <span className="details-workspace-title">{tr('Member workspaces')} <span className={`disclosure-chevron${expanded ? ' open' : ''}`}><Icon name="chevron.right" size={10} /></span></span>
        <small>{tr('Includes project files and build caches')}</small>
      </span>
      <strong>{total}</strong>
    </button>
    <Reveal show={expanded}>
      {usage?.members.length ? <ul id={listId} className="details-workspace-list" tabIndex={0} aria-label={tr('Member workspaces')}>
        {usage.members.map(member => {
          const directory = local ? member.directory : undefined
          return <li key={member.botId}>
            <span className="details-workspace-label">
              <span className="details-workspace-name" title={member.name}>{member.name}</span>
              {member.shared && <small>{tr('Shared across groups')}</small>}
            </span>
            <span>{member.bytes == null ? tr('Unavailable') : bytes(member.bytes)}</span>
            {directory && <IconButton icon="folder" title={tr('Open folder')} disabled={!online} onClick={() => { /* 只打开本机 Host 提供的目录。 */ void openWorkspace(store, directory, tr) }} />}
          </li>
        })}
      </ul> : null}
    </Reveal>
  </div>
}
