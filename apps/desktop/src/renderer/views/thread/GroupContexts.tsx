import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { isGroup, type Bot } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { Button, CardForm, CardSection } from '../../components/Controls'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { Icon } from '../../components/Icon'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { avatarColor } from '../../lib/theme'
import { useApp, useStore } from '../../store/context'
import { DeliveryNotice } from './DeliveryNotice'
import { SessionHistory } from './SessionHistory'
import type { Entry } from '@shared/models'
import { Field } from '../marketplace/pieces'
import './group-contexts.css'

type Basis = 256000 | 512000 | 1000000
type Review = { phase?: string; deliveryId?: string; privateEntryId?: string; text?: string; revision?: string }
type MemberContext = {
  botId: string
  hasSession: boolean
  used?: number | null
  size?: number | null
  actualModelName?: string | null
  reasoning?: string | null
  shared: boolean
  recommended: boolean
  handoff?: Review | null
}
type Snapshot = { basis: Basis; members: MemberContext[] }
type Draft = { botId: string; revision: string; deliveryId?: string; text: string }
type Context = {
  panelVisible: boolean
  snapshot: Snapshot | null
  disabled: boolean
  error: string | null
  review: Draft | null
  inspect: (botId: string) => void
  setBasis: (basis: Basis) => Promise<void>
  openReview: (member: MemberContext) => Promise<void>
  updateReview: (text: string) => void
  closeReview: () => void
  confirm: () => Promise<void>
}
const bases = [256000, 512000, 1000000] as const
const Current = createContext<Context | null>(null)

/** 千位使用十进制，详情另显示精确 Token 数。 */
function compact(tokens: number) {
  return tokens === 1000000 ? '1M' : `${Number((tokens / 1000).toFixed(1))}K`
}

/** 统一计算成员行和聊天名字旁的占比，未知用量不冒充零。 */
function percentage(used: number | null | undefined, total: number) {
  return used == null ? null : `${(used / total * 100).toFixed(1)}%`
}

/** 成员列表和聊天名字旁共用一份用量快照，关闭侧栏仍可看消息标签。 */
export function GroupContexts({ room, panelVisible, openPanel, children }: {
  room: Bot | null
  panelVisible: boolean
  openPanel: () => void
  children: ReactNode
}) {
  const store = useStore()
  const tr = useTr()
  const app = useApp()
  const roomId = room && isGroup(room) && store.hello?.groupSessionContexts === true ? room.id : null
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [inspected, setInspected] = useState<string | null>(null)
  const [review, setReview] = useState<Draft | null>(null)
  const reviewRequest = useRef(0)
  const wasPanelVisible = useRef(panelVisible)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [refreshError, setRefreshError] = useState<string | null>(null)
  const online = roomId !== null && !store.isOffline && !store.mismatch
  const members = roomId && room ? store.members(room) : []
  const stamp = [room?.rev, ...members.map(bot => `${bot.id}:${bot.rev}`)].join('|')
  const current = snapshot?.members.find(member => member.botId === inspected)

  useEffect(() => {
    if (!roomId || !online) return
    let valid = true
    // 合并同一次回合发来的成员、群和整理事件。
    const timer = setTimeout(async () => {
      try {
        const result = await (await store.ready()).call<Snapshot>('groupSessionContexts', { roomId })
        if (valid) { setSnapshot(result); setRefreshError(null) }
      } catch (failure) {
        if (valid) setRefreshError(String(failure instanceof Error ? failure.message : failure))
      }
    }, 90)
    return () => { valid = false; clearTimeout(timer) }
  }, [store, roomId, stamp, online])

  // 从消息头打开侧栏不能使正在准备的摘要失效；仅关闭现有侧栏时取消。
  useLayoutEffect(() => {
    if (wasPanelVisible.current && !panelVisible) {
      reviewRequest.current += 1
      setReview(null)
      setError(null)
    }
    wasPanelVisible.current = panelVisible
  }, [panelVisible])

  useLayoutEffect(() => {
    // 切群或卸载后不再接受旧群的摘要响应，也不跳到无关私聊。
    return () => { reviewRequest.current += 1 }
  }, [roomId])

  /** 档位由 Host 保存，成员列表和聊天共用同一个响应。 */
  async function setBasis(basis: Basis) {
    if (!roomId) return
    setBusy(true)
    setError(null)
    try {
      const result = await (await store.ready()).call<Snapshot>('setGroupContextBasis', { roomId, basis })
      setSnapshot(result)
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally { setBusy(false) }
  }

  /** 优先审阅原派工的交接，共享摘要跳到对应私聊卡。 */
  async function openReview(member: MemberContext) {
    if (!roomId || !online || busy) return
    openPanel()
    const request = ++reviewRequest.current
    setBusy(true)
    setError(null)
    try {
      const client = await store.ready()
      const result = await client.call<Review>('prepareGroupMemberSession', { roomId, botId: member.botId })
      if (reviewRequest.current !== request) return
      if (result.privateEntryId) {
        app.select({ computerId: store.computer.id, botId: member.botId }, result.privateEntryId)
        return
      }
      if (result.phase === 'approved' || result.phase === 'switched') return
      const draft = result.deliveryId ? await store.groupHandoff(result.deliveryId) : result
      if (reviewRequest.current === request && draft.revision && draft.text != null) {
        setInspected(null)
        setReview({ botId: member.botId, revision: draft.revision, text: draft.text, deliveryId: result.deliveryId })
      }
    } catch (failure) {
      if (reviewRequest.current === request) setError(String(failure instanceof Error ? failure.message : failure))
    } finally { setBusy(false) }
  }

  /** 原派工和主动新开使用各自的身份，失败时保留改过的摘要。 */
  async function confirm() {
    if (!roomId || !online || busy || !review?.text.trim()) return
    setBusy(true)
    setError(null)
    try {
      if (review.deliveryId) await store.confirmGroupHandoff(review.deliveryId, review.revision, review.text)
      else await (await store.ready()).call('confirmGroupMemberSession', {
        roomId, botId: review.botId, revision: review.revision, text: review.text,
      })
      setReview(null)
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally { setBusy(false) }
  }

  /** 编辑失败时保留草稿，切换房间时由房间组件释放。 */
  function updateReview(text: string) {
    if (review) setReview({ ...review, text })
  }

  /** 取消审阅回到成员列表，清除本次操作错误。 */
  function closeReview() {
    reviewRequest.current += 1
    setReview(null)
    setError(null)
  }

  return <Current.Provider value={roomId ? { panelVisible, snapshot, inspect: setInspected, disabled: busy || !online, error: error ?? refreshError,
    review, setBasis, openReview, updateReview, closeReview, confirm } : null}>
    {children}
    <Sheet open={current != null} onClose={() => setInspected(null)} width={420}>
      {current ? <SheetPage>
        <ModalHeader title={store.bots.get(current.botId)?.name ?? tr('Member context')} />
        <CardForm>
          {roomId && store.hello?.groupMemberActivity && <MemberActivity key={current.botId} roomId={roomId} botId={current.botId} onJump={() => {
            // 先关闭成员弹层，焦点再回到群聊里的原派工。
            setInspected(null)
          }} />}
          {!current.shared && roomId && store.hello?.sessionHistory && <SessionHistory botId={current.botId} roomId={roomId} />}
          <CardSection title={tr('Current context')}>
            <Text size="body">{current.used != null ? `${current.used.toLocaleString()} token` : tr(current.hasSession ? 'Usage not reported' : 'No session yet')}</Text>
            <Text size="footnote" tone="secondary">{tr('Display basis')} · {compact(snapshot?.basis ?? 512000)} · {percentage(current.used, snapshot?.basis ?? 512000) ?? '—'}</Text>
            <Text size="footnote" tone="secondary">{tr('Actual model window')} · {current.size ? compact(current.size) : '—'} · {current.size ? percentage(current.used, current.size) ?? '—' : '—'}</Text>
            {current.shared ? <Text size="footnote" tone="secondary">{tr('This also starts a new private session')}</Text> : null}
          </CardSection>
        </CardForm>
      </SheetPage> : null}
    </Sheet>
  </Current.Provider>
}

/** 从成员详情读取原派工，随后以群聊实时事件刷新同一张卡。 */
function MemberActivity({ botId, roomId, onJump }: { botId: string; roomId: string; onJump: () => void }) {
  const tr = useTr()
  const store = useStore()
  const [result, setResult] = useState<{ entry: Entry | null; deliveryId: string | null } | null>(null)
  const [error, setError] = useState(false)
  const online = !store.isOffline && !store.mismatch
  const revision = store.bots.get(botId)?.rev
  useEffect(() => {
    if (!online) return
    let current = true
    void store.ready().then(client => client.call<{ entry: Entry | null; deliveryId: string | null }>('groupMemberActivity', { botId, roomId })).then(
      value => { if (current) { setResult(value); setError(false) } },
      () => { if (current) setError(true) },
    )
    return () => { current = false }
  }, [store, botId, roomId, online, revision])
  const entry = result?.entry
  const latest = entry ? store.allEntries(roomId).find(item => item.id === entry.id) ?? entry : null
  /** 已载入的卡片回到群聊原位，卸载的旧卡片仍可在此查看。 */
  function jump() {
    if (!latest) return
    const target = document.getElementById(latest.id)
    if (!target) return
    onJump()
    requestAnimationFrame(() => {
      // 展开原派工并让键盘焦点从弹页回到准确位置。
      const disclosure = target.querySelector<HTMLButtonElement>('button[aria-expanded]')
      if (disclosure?.getAttribute('aria-expanded') === 'false') disclosure.click()
      target.scrollIntoView({ block: 'center' })
      disclosure?.focus({ preventScroll: true })
    })
  }

  return <CardSection title={tr('Member activity')}>
    {latest && store.allEntries(roomId).some(entry => entry.id === latest.id) && <Button kind="secondary" onClick={jump}>{tr('Open original assignment')}</Button>}
    {latest && result?.deliveryId ? <DeliveryNotice entry={latest} deliveryId={result.deliveryId} />
      : <Text tone="secondary">{tr(!online || error ? 'Unavailable' : result ? 'No assignments yet' : 'Loading…')}</Text>}
  </CardSection>
}

/** 侧栏使用群共享的上下文和摘要草稿。 */
export function useGroupContexts() {
  return useContext(Current)
}

/** 在现有成员列表的标题行设置全群显示基准。 */
export function GroupContextHeading() {
  const state = useGroupContexts()
  const tr = useTr()
  if (!state) return null
  return <div className="group-context-heading">
    <div className="group-context-title">
      <Text size="footnote" weight="semibold">{tr('Member context')}</Text>
      {state.snapshot?.members[0]?.shared ? <Text size="caption" tone="secondary">{tr('Shares private sessions')}</Text> : null}
    </div>
    <div className="group-context-bases" aria-label={tr('Display basis')} role="group">
      {bases.map(basis => <button key={basis} type="button" aria-pressed={(state.snapshot?.basis ?? 512000) === basis}
        disabled={state.disabled} onClick={() => { /* 只更新全群显示档位。 */ void state.setBasis(basis) }}>{compact(basis)}</button>)}
    </div>
  </div>
}

/** 成员行和消息头共用推荐入口，确认后的等待状态不允许重复操作。 */
export function GroupContextAction({ botId }: { botId: string }) {
  const state = useGroupContexts()
  const tr = useTr()
  const member = state?.snapshot?.members.find(row => row.botId === botId)
  if (!state || !member) return null
  if (member.handoff?.phase === 'approved' || member.handoff?.phase === 'switched') {
    return <Text size="caption" tone="secondary">{tr('New session after this turn')}</Text>
  }
  if (!member.recommended && member.handoff?.phase !== 'draft') return null
  return <Button className="group-context-action" kind="secondary" disabled={state.disabled}
    title={tr(member.shared ? 'This also starts a new private session' : 'Summary for the new session')}
    onClick={() => { /* 普通群的摘要打开对应私聊。 */ void state.openReview(member) }}><Icon name="arrow.clockwise" size={12} /> {tr('Open new session')}</Button>
}

/** 成员行的进度条跟随显示档，点读数可查看精确用量与实际模型窗口。 */
export function GroupContextReading({ bot }: { bot: Bot }) {
  const state = useGroupContexts()
  const tr = useTr()
  if (!state) return null
  const member = state.snapshot?.members.find(row => row.botId === bot.id)
  const total = state.snapshot?.basis ?? 512000
  const percent = percentage(member?.used, total)
  return <button type="button" className="group-context-reading" disabled={!member}
    onClick={() => { /* 读数与机器人私聊入口独立。 */ state.inspect(bot.id) }}>
    <span>
      <strong>{member?.used == null ? '—' : compact(member.used)}</strong>
      <small>{percent ?? tr(member?.hasSession === false ? 'No session yet' : member ? 'Usage not reported' : 'Unavailable')}</small>
    </span>
    {member?.used != null ? <span className="group-context-meter" aria-hidden="true">
      <span style={{ background: member.recommended ? 'var(--warning)' : avatarColor(bot.avatarColor), width: `${Math.min(100, member.used / total * 100)}%` }} />
    </span> : null}
  </button>
}

/** 独立群在原侧栏编辑摘要，消息和原派工可继续查看。 */
export function GroupContextReview() {
  const state = useGroupContexts()
  const store = useStore()
  const tr = useTr()
  const review = state?.review
  if (!state || !review) return null
  const member = state.snapshot?.members.find(row => row.botId === review.botId)
  const bot = store.bots.get(review.botId)
  return <section className="group-context-review" aria-label={tr('Open new session')}>
    <Button kind="secondary" className="group-context-back" onClick={state.closeReview}>
      <Icon name="chevron.left" size={11} /> {tr('Members')}
    </Button>
    <Text as="h2" className="group-context-review-title" size="title3" weight="semibold">{tr('Open new session')}</Text>
    <div className="group-context-review-member">
      {bot ? <BotAvatar bot={bot} size={32} animated={false} /> : null}
      <div>
        {bot ? <Text as="div" size="body" weight="semibold">{bot.name}</Text> : null}
        {member?.size ? <Text size="caption" tone="secondary">
          {tr('Actual model window')} · {member.used == null ? '—' : compact(member.used)} / {compact(member.size)} · {percentage(member.used, member.size) ?? '—'}
        </Text> : null}
      </div>
    </div>
    <Text as="div" size="footnote" weight="semibold">{tr('Summary for the new session')}</Text>
    <Field label="Summary for the new session" value={review.text} onChange={state.updateReview} lines={[7, 15]} />
    <Text size="caption" tone="secondary">{tr('The new session starts after the current turn.')}</Text>
    {state.error ? <Text size="footnote">{state.error}</Text> : null}
    <div className="group-context-review-actions">
      <Button kind="secondary" onClick={state.closeReview}>Cancel</Button>
      <Button disabled={state.disabled || !review.text.trim()} onClick={() => { /* 仅确认才提交摘要。 */ void state.confirm() }}>Confirm new session</Button>
    </div>
  </section>
}

/** 最近一条成员消息显示实时读数、模型和新开入口，历史消息不重复堆按钮。 */
export function GroupContextChip({ botId }: { botId?: string | null }) {
  const tr = useTr()
  const state = useContext(Current)
  const snapshot = state?.snapshot
  const member = snapshot?.members.find(row => row.botId === botId)
  if (!state || !snapshot || !member) return null
  return <>
    {member.used != null && <button type="button" className={`group-context-chip${member.recommended ? ' recommended' : ''}`} onClick={() => { /* 只点用量打开当前会话详情。 */ state.inspect(member.botId) }}>
      {compact(member.used)} · {percentage(member.used, snapshot.basis)}
    </button>}
    {member.actualModelName && <span className="group-context-model">
      <span title={member.actualModelName}>{member.actualModelName}</span>
      {member.reasoning && <span className="group-context-effort">{tr(member.reasoning)}</span>}
    </span>}
    <GroupContextAction botId={member.botId} />
  </>
}
