import { useEffect, useState } from 'react'
import { needsInput, type Bot, type Entry } from '@shared/models'
import { Reveal } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { ThinkingOrb } from '../../components/ThinkingOrb'
import { prefs, usePref } from '../../lib/prefs'
import { useTr } from '../../lib/use-language'
import { useFollowEnd } from './follow-end'
import { TraceRow } from './TraceView'
import { workingSteps } from './trace-turns'

/** 展示当前活动和用时，开启思考详情时可展开这次执行的步骤。 */
export function WorkingIndicator({ bot, entries, group }: { bot: Bot; entries: Entry[]; group: boolean }) {
  const tr = useTr()
  const [details] = usePref(prefs.thinkingDetails)
  const waiting = needsInput(bot)
  const [expanded, setExpanded] = useState(false)
  const open = details && expanded
  const activity = tr(bot.activity || 'Working…')
  const member = group && bot.kind !== 'group'
  const tone = waiting ? 'attention' : 'secondary'
  const status = (
    <HStack as="span" gap={8}>
      <ThinkingOrb state={waiting ? 'listening' : 'working'} size={16} color={waiting ? 'var(--warning)' : 'var(--secondary)'} />
      {member ? (
        <HStack as="span" grow className="working-label">
          <Text size="subheadline" tone={tone} truncate>{bot.name}:&nbsp;</Text>
          <Text size="subheadline" tone={tone} truncate>{activity}</Text>
        </HStack>
      ) : <Text size="subheadline" tone={tone} truncate grow>{activity}</Text>}
      {bot.startedAt ? <Timer since={bot.startedAt} /> : null}
      {details ? (
        <span className={`disclosure-chevron${open ? ' open' : ''}`}>
          <Icon name="chevron.right" size={10} weight="semibold" />
        </span>
      ) : null}
    </HStack>
  )
  return (
    <VStack align="flex-start" className={group ? 'group-working-member' : 'agent-inset'}>
      <div className={open ? 'working expanded' : 'working'}>
        {details ? (
          <button className="working-toggle" onClick={() => setExpanded((x) => !x)} aria-expanded={open} title={tr(open ? 'Hide steps' : 'Show steps')}>
            {status}
          </button>
        ) : (
          status
        )}
        <Reveal show={open}>
          <WorkingSteps entries={entries} member={member ? bot : undefined} />
        </Reveal>
      </div>
    </VStack>
  )
}

/** 展开时读取并跟随当前回合的步骤。群回合只读取该成员本次执行的记录。 */
function WorkingSteps({ entries, member }: { entries: Entry[]; member?: Bot }) {
  const tr = useTr()
  const follow = useFollowEnd()
  const steps = workingSteps(member ? entries.filter(
    // 同群旧回合和其他成员的内容留在完整会话。
    (entry) => entry.data.author === member.id && entry.createdAt >= (member.startedAt ?? Infinity),
  ) : entries)
  return (
    <div ref={follow.ref} className="working-steps" onScroll={follow.onScroll}>
      <VStack gap={10}>
        {steps.length === 0 ? <Text tone="tertiary">{tr('Nothing yet.')}</Text> : null}
        {steps.map((e) => <TraceRow key={e.id} entry={e} unfolded />)}
      </VStack>
    </div>
  )
}

/** `Text(date, style: .timer)`: 0:42, 12:05, 1:02:33. */
function Timer({ since }: { since: number }) {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [])
  const s = Math.max(0, Math.floor((now - since) / 1000))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = String(s % 60).padStart(2, '0')
  return <Text size="footnote" tone="tertiary" tabular>{h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`}</Text>
}
