import { useEffect, useState } from 'react'
import { hasLibrary } from '@shared/artifacts'
import { isGroup, isWorking, type Bot, type BotSessionUsage } from '@shared/models'
import { AgentIcon } from '../../components/AgentIcon'
import { BotAvatar, GroupAvatar } from '../../components/Avatar'
import { Button, CardSection, Disclosure, IconButton, RowLabel, Switch, TabChoice } from '../../components/Controls'
import { LeadAgentBadge } from '../../components/LeadAgentBadge'
import { Icon } from '../../components/Icon'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { useScreenViewed } from '../../store/analytics'
import { BotSettingsPanel } from '../bots/BotEditorView'
import { MemoryCard } from '../bots/MemoryView'
import { RoutinesView } from '../routines/RoutinesView'
import { LibraryView } from '../library/LibraryView'
import { DevServersSection } from './DevServersSection'
import { GroupContextAction, GroupContextHeading, GroupContextReading, GroupContextReview, useGroupContexts } from './GroupContexts'
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
          reviewing ? <GroupContextReview /> : <GroupMembersList group={bot} editGroup={editGroup} />
        ) : (
          <>
            <TabChoice label="Conversation details" selection={tab} options={hasLibrary(store.hostVersion?.version) ? TABS : TABS.filter((t) => t.id !== 'library')} onChange={setTab} />
            <div key={tab} className="details-tab fade-in">
              {tab === 'details' ? (
                <>
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
                {isWorking(bot) ? <Text size="compactSecondary" tone="secondary" className="member-name">{tr(bot.activity || 'Working…')}</Text> : null}
              </VStack>
            </button>
            <GroupContextAction botId={bot.id} />
            {group.members.length > 1 ? <IconButton className="member-remove"
              title={tr('Remove {name} from the group').replaceAll('{name}', () => bot.name)} icon="xmark"
              onClick={() => { /* 移除入口与会话操作各自独立。 */ store.removeMember(group, bot.id) }} /> : null}
          </div>
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
