import { useState } from 'react'
import type { DeliveryRow, Entry } from '@shared/models'
import { Button, CardForm, CardSection, Disclosure, Spinner } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useApp, useStore } from '../../store/context'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { Field } from '../marketplace/pieces'
import './handoff.css'
import { useTr } from '../../lib/use-language'
import { deliveryStatus, deliverySummary } from './delivery-status'
import { FileDownloadCard } from './FileDownloadCard'

type ResultPreview = { entry: Entry; evidence?: Entry }

/** 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。 */
export function DeliveryNotice({ entry, deliveryId }: { entry: Entry; deliveryId?: string }) {
  const store = useStore()
  const tr = useTr()
  const [expanded, setExpanded] = useState(deliveryId != null)
  const rows = (entry.data.deliveryRows ?? []).filter(row => deliveryId == null || row.deliveryId === deliveryId)
  /** 只显示同群中已声明支持引导的当前回合，提交时仍由 host 复核。 */
  const activeId = (row: DeliveryRow) => {
    const bot = store.bots.get(row.targetBotId)
    return row.canGuide && row.status === 'queued' && store.connection.kind === 'online'
      && bot?.status === 'working' && !bot.storageError && bot.workingChat === entry.botId ? bot.guidingDeliveryId : null
  }
  // 保留渲染时的回合身份，点击迟到时由 host 拒绝旧操作。
  const eligible = rows.flatMap((row) => {
    const id = activeId(row)
    return id ? [{ row, id }] : []
  })
  // 顶部有操作入口时，原派工折叠成一行状态，展开仍可看每位成员的结果。
  const phase = store.hello?.groupSessionContexts
    ? ['draft', 'approved', 'switched', 'sent'].find(value => rows.some(row => row.groupHandoff?.phase === value))
    : undefined
  const host = rows.some(row => row.executor === 'host')
  const summary = host ? `${tr('Host execution')} · ${deliverySummary(rows)}` : phase === 'draft' ? tr('Waiting for a new session')
    : phase === 'sent' ? tr('New session started. Original dispatch resumed.')
    : phase ? tr('Preparing a new session') : deliverySummary(rows)
  return <HStack id={deliveryId == null ? entry.id : undefined} className="delivery-notice" align="flex-start" gap={8}>
    <VStack grow>
      {/* 复用折叠控件的键盘与 aria-expanded 行为，正文保留在原消息中。 */}
      <Disclosure expanded={expanded} onToggle={() => setExpanded(!expanded)} label={<Text size="footnote" tone="secondary" truncate>{summary}</Text>}>
        <VStack gap={8} className="fade-in delivery-details">
          <Text size="footnote" tone="secondary">第 {entry.data.deliveryAttempt ?? 1} 次投递{entry.data.parentDeliveryId ? ' · 重试投递' : ''}</Text>
          {rows.map((row) => {
            // 显式引导只作用于该行成员，重试仍由 host 新建投递。
            const id = activeId(row)
            return <VStack key={row.deliveryId} gap={5}>
              <HStack gap={8}>
              <Text size="footnote" tone="secondary" grow>{row.executor === 'host' ? tr('Host execution') : store.bots.get(row.targetBotId)?.name ?? '已移除成员'}：{deliveryStatus(row.status)}{row.detail ? ` · ${row.detail}` : ''}</Text>
              {id
                ? <Button kind="secondary" onClick={() => store.guideDelivery(row.deliveryId, id)}>引导</Button>
                : (row.executor !== 'host' || store.hello?.groupLocalTasks) && ['failed', 'interrupted', 'noReply'].includes(row.status)
                  ? <Button kind="secondary" onClick={() => store.retryDelivery(row.deliveryId)}>重新尝试</Button> : null}
              {row.executor === 'host' && store.hello?.groupLocalTasks && row.startedAt != null && ['started', 'paused'].includes(row.status)
                ? <Button kind="secondary" onClick={() => store.setDeliveryPaused(entry.botId, row.deliveryId, row.startedAt!, row.status === 'started')}>{tr(row.status === 'started' ? 'Pause' : 'Continue')}</Button> : null}
              </HStack>
              <DeliveryActivity row={row} roomId={entry.botId} inDetails={deliveryId != null} />
            </VStack>
          })}
        </VStack>
      </Disclosure>
    </VStack>
    {!expanded && eligible.length > 0
      ? <Button kind="secondary" onClick={() => {
          // 多个成员可以接收引导时，先展开以便明确选择目标。
          if (eligible.length === 1 && eligible[0]) {
            const { row, id } = eligible[0]
            void store.guideDelivery(row.deliveryId, id)
          }
          else setExpanded(true)
        }}>{eligible.length === 1 ? '引导' : '引导…'}</Button> : null}
    {!store.hello?.groupSessionContexts && rows.filter(row => row.groupHandoff).map(row => <GroupHandoffRow row={row} key={row.deliveryId} />)}
  </HStack>
}

/** 在原卡回看步骤，结果优先定位到群聊原消息。 */
function DeliveryActivity({ row, roomId, inDetails }: { row: DeliveryRow; roomId: string; inDetails: boolean }) {
  const tr = useTr()
  const store = useStore()
  const [result, setResult] = useState<ResultPreview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  /** 旧结果尚未载入或原卡处于详情弹层时，读取同群的原消息。 */
  async function openResult() {
    if (!row.resultId) return
    const target = inDetails ? null : document.getElementById(row.resultId)
    if (target) {
      target.scrollIntoView({ block: 'center' })
      target.focus({ preventScroll: true })
      return
    }
    setLoading(true)
    setError(null)
    try {
      setResult(await (await store.ready()).call<ResultPreview>('groupDeliveryResult', { roomId, resultId: row.resultId }))
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure))
    } finally {
      setLoading(false)
    }
  }

  /** 显示设备本地时间，便于与聊天消息对应。 */
  const time = (at: number) => new Date(at).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  return <VStack gap={4}>
    {row.startedAt != null && <Text size="footnote" tone="secondary">{time(row.startedAt)} · {tr('Started assignment')}</Text>}
    {(row.activity ?? []).map(step => {
      // 结束的派工可能保留未结束的工具，仍显示历史上的开始时间。
      const status = step.status === 'completed' ? 'Step completed' : step.status === 'failed' ? 'Step failed' : step.status === 'interrupted' ? 'Step interrupted'
        : row.status === 'started' ? 'In progress' : 'Step started'
      return <Text key={step.id} size="footnote" tone={status === 'In progress' ? undefined : 'secondary'}>{time(step.at)} · {tr(step.label)} · {tr(status)}</Text>
    })}
    {row.resultId && <Button kind="secondary" disabled={loading} onClick={() => { /* 打开已保存的群聊结果。 */ void openResult() }}>{tr('View result')}</Button>}
    {error && <Text size="footnote" tone="secondary" role="status">{error}</Text>}
    <Sheet open={result != null} onClose={() => { /* 关闭结果页后仍可查看原派工。 */ setResult(null) }}>
      {result && <SheetPage><ModalHeader title={tr('View result')} /><CardForm>
        <Text>{result.entry.data.text}</Text>
        {result.evidence && (() => {
          // 附件仍向 Host 请求其原始消息 ID，不依赖群聊天的分页缓存。
          const evidence = result.evidence
          return evidence.data.files?.map(file => <FileDownloadCard key={file.id} entry={evidence} file={file}
            messageItems={() => []} />)
        })()}
      </CardForm></SheetPage>}
    </Sheet>
  </VStack>
}

/** 操作跟在原派工下，普通群仅显示作用域并打开对应私聊。 */
function GroupHandoffRow({ row }: { row: DeliveryRow }) {
  const tr = useTr()
  const app = useApp()
  const store = useStore()
  const [revision, setRevision] = useState('')
  const [draft, setDraft] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const handoff = row.groupHandoff
  const name = store.bots.get(row.targetBotId)?.name ?? '已移除成员'

  /** 打开原派工的摘要，普通群沿用机器人私聊中的交接卡。 */
  async function review() {
    if (handoff?.shared) {
      app.select({ computerId: store.computer.id, botId: row.targetBotId })
      return
    }
    setBusy(true)
    setError(null)
    try {
      const result = await store.groupHandoff(row.deliveryId)
      setRevision(result.revision)
      setDraft(result.text)
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally {
      setBusy(false)
    }
  }

  /** 保存摘要与确认同一次提交，失败保留输入。 */
  async function confirm() {
    if (!draft?.trim()) return
    setBusy(true)
    setError(null)
    try {
      await store.confirmGroupHandoff(row.deliveryId, revision, draft)
      setDraft(null)
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally {
      setBusy(false)
    }
  }
  if (!handoff) return null
  if (handoff.phase === 'sent') {
    return <Text size="footnote" tone="secondary">{name} · {tr("New session started. Original dispatch resumed.")}</Text>
  }
  if (handoff.phase !== 'draft') {
    return <Text size="footnote" tone="secondary">{name} · {tr("Preparing a new session")}</Text>
  }
  return <div className="handoff-card delivery-handoff">
    <HStack gap={8}><Text size="footnote" weight="semibold">{name}</Text><span className="handoff-status">{tr("Session switch needs confirmation")}</span></HStack>
    <Text size="footnote" tone="secondary">{tr("The task still won't fit after compaction. It will resume after you confirm.")}</Text>
    {handoff.shared ? <Text size="footnote" tone="secondary">{tr("This also switches the bot's private session.")}</Text> : null}
    <HStack gap={8}><Button disabled={busy || store.isOffline} onClick={() => { void review() }}>
      {handoff.shared ? 'Review handoff in private chat' : 'Review handoff and switch session'}
    </Button></HStack>
    {error && draft === null ? <Text size="footnote">{error}</Text> : null}
    <Sheet open={draft !== null} onClose={() => setDraft(null)} width={560}>
      {draft !== null ? <SheetPage>
        <ModalHeader title={tr("Review handoff and switch session")} trailing={busy ? <Spinner /> : undefined} />
        <CardForm>
          <CardSection title={name} footer="This switches the bot's session in this group. Group history and pending tasks are kept.">
            <Field label="Summary for the new session" value={draft} onChange={setDraft} lines={[8, 16]} />
          </CardSection>
          {error ? <Text size="footnote">{error}</Text> : null}
          <HStack gap={8}>
            <Button kind="secondary" disabled={busy} onClick={() => setDraft(null)}>Not now</Button>
            <Button disabled={busy || !draft.trim()} onClick={() => { void confirm() }}>Confirm new session</Button>
          </HStack>
        </CardForm>
      </SheetPage> : null}
    </Sheet>
  </div>
}

/** 从机器人名称跳到该成员当前等待确认的原派工。 */
export function GroupHandoffBadge({ cardId }: { cardId: string }) {
  const tr = useTr()
  return <button type="button" className="handoff-status" onClick={() => document.getElementById(cardId)?.scrollIntoView({ block: 'center' })}>{tr("Session switch needs confirmation")}</button>
}
