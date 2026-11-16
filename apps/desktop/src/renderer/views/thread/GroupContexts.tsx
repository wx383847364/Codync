import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import { isGroup, type Bot } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { Button, CardForm, CardSection, Spinner } from '../../components/Controls'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { avatarColor } from '../../lib/theme'
import { useApp, useStore } from '../../store/context'
import { Field } from '../marketplace/pieces'
import './group-contexts.css'

type Basis = 256000 | 512000 | 1000000
type Review = { phase?: string; deliveryId?: string; privateEntryId?: string; text?: string; revision?: string }
type MemberContext = {
  botId: string
  hasSession: boolean
  used?: number | null
  size?: number | null
  shared: boolean
  recommended: boolean
  handoff?: Review | null
}
type Snapshot = { basis: Basis; members: MemberContext[] }
const bases = [256000, 512000, 1000000] as const
const Current = createContext<{ snapshot: Snapshot | null; inspect: (botId: string) => void } | null>(null)

/** 千位使用十进制，详情另显示精确 Token 数。 */
function compact(tokens: number) {
  return tokens === 1000000 ? '1M' : `${Number((tokens / 1000).toFixed(1))}K`
}

/** 统一计算卡片和聊天名字旁的占比，未知用量不冒充零。 */
function percentage(used: number | null | undefined, total: number) {
  return used == null ? null : `${(used / total * 100).toFixed(1)}%`
}

/** 会员用量在群内共用一次读取，菜单和聊天气泡读取同一快照。 */
export function GroupContexts({ room, children }: { room: Bot | null; children: ReactNode }) {
  const store = useStore()
  const enabled = !!room && isGroup(room) && store.hello?.groupSessionContexts === true
  if (!enabled) return <>{children}</>
  return <GroupContextsActive key={room.id} room={room}>{children}</GroupContextsActive>
}

/** 会话同步和确认面板由当前群承载，切换群自然关闭面板。 */
function GroupContextsActive({ room, children }: { room: Bot; children: ReactNode }) {
  const tr = useTr()
  const app = useApp()
  const store = useStore()
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [inspected, setInspected] = useState<string | null>(null)
  const [review, setReview] = useState<{ botId: string; revision: string; deliveryId?: string; text: string } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [refreshError, setRefreshError] = useState<string | null>(null)
  const online = !store.isOffline && !store.mismatch
  const members = store.members(room)
  const stamp = [room.rev, ...members.map(bot => `${bot.id}:${bot.rev}`)].join('|')
  const current = snapshot?.members.find(member => member.botId === inspected)

  useEffect(() => {
    if (!online) return
    let valid = true
    // 合并同一次回合发来的成员、群和整理事件。
    const timer = setTimeout(async () => {
      try {
        const result = await (await store.ready()).call<Snapshot>('groupSessionContexts', { roomId: room.id })
        if (valid) { setSnapshot(result); setRefreshError(null) }
      } catch (failure) {
        if (valid) setRefreshError(String(failure instanceof Error ? failure.message : failure))
      }
    }, 90)
    return () => { valid = false; clearTimeout(timer) }
  }, [store, room.id, stamp, online])

  /** 档位由 Host 保存，顶部和聊天共用同一个响应。 */
  async function setBasis(basis: Basis) {
    setBusy(true)
    setError(null)
    try {
      const result = await (await store.ready()).call<Snapshot>('setGroupContextBasis', { roomId: room.id, basis })
      setSnapshot(result)
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally { setBusy(false) }
  }

  /** 优先审阅原派工的交接，共享摘要跳到对应私聊卡。 */
  async function openReview(member: MemberContext) {
    setBusy(true)
    setError(null)
    try {
      const client = await store.ready()
      const result = await client.call<Review>('prepareGroupMemberSession', { roomId: room.id, botId: member.botId })
      if (result.privateEntryId) {
        app.select({ computerId: store.computer.id, botId: member.botId }, result.privateEntryId)
        return
      }
      if (result.phase === 'approved' || result.phase === 'switched') return
      const draft = result.deliveryId ? await store.groupHandoff(result.deliveryId) : result
      if (draft.revision && draft.text != null) {
        setInspected(null)
        setReview({ botId: member.botId, revision: draft.revision, text: draft.text, deliveryId: result.deliveryId })
      }
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally { setBusy(false) }
  }

  /** 原派工和主动新开使用各自的身份，失败时保留改过的摘要。 */
  async function confirm() {
    if (!review?.text.trim()) return
    setBusy(true)
    setError(null)
    try {
      if (review.deliveryId) await store.confirmGroupHandoff(review.deliveryId, review.revision, review.text)
      else await (await store.ready()).call('confirmGroupMemberSession', {
        roomId: room.id, botId: review.botId, revision: review.revision, text: review.text,
      })
      setReview(null)
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure))
    } finally { setBusy(false) }
  }

  return <Current.Provider value={{ snapshot, inspect: setInspected }}>
    <section className="group-contexts" aria-label={tr('Member context')}>
      <div className="group-context-inner">
        <div className="group-context-heading">
          <span className="group-context-title"><Text size="footnote" weight="semibold">{tr('Member context')}</Text>
            {snapshot?.members[0]?.shared ? <Text size="caption" tone="secondary">{tr('Shares private sessions')}</Text> : null}</span>
          <div className="group-context-bases" aria-label={tr('Display basis')} role="group">
            <Text size="caption" tone="secondary">{tr('Display basis')}</Text>
            {bases.map(basis => <button key={basis} type="button" aria-pressed={(snapshot?.basis ?? 512000) === basis}
              disabled={busy || !online} onClick={() => { void setBasis(basis) }}>{compact(basis)}</button>)}
          </div>
        </div>
        <div className="group-context-members">
          {members.map(bot => {
            const member = snapshot?.members.find(context => context.botId === bot.id)
            const total = snapshot?.basis ?? 512000
            const percent = percentage(member?.used, total)
            const waiting = member?.handoff?.phase === 'approved' || member?.handoff?.phase === 'switched'
            const canReview = member && (member.recommended || member.handoff?.phase === 'draft')
            return <div className="group-context-card" key={bot.id}>
              <div className="group-context-card-head">
                <BotAvatar bot={bot} size={19} animated={false} />
                <Text size="caption" weight="semibold" truncate grow>{bot.name}</Text>
                {waiting ? <Text size="caption" tone="secondary">{tr('New session after this turn')}</Text> : canReview ?
                  <Button kind="secondary" disabled={busy || !online} title={member.shared ? 'This also starts a new private session' : undefined}
                    onClick={() => { void openReview(member) }}>Open new session</Button> : null}
              </div>
              <button type="button" className="group-context-reading" onClick={() => setInspected(bot.id)} disabled={!member}>
                <span><strong>{member?.used == null ? '—' : compact(member.used)}</strong>
                  <small>{percent ?? tr(member?.hasSession === false ? 'No session yet' : 'Usage not reported')}</small></span>
                <span className="group-context-meter"><span style={{ background: avatarColor(bot.avatarColor), width: percent === null ? 0 : `${Math.min(100, (member?.used ?? 0) / total * 100)}%` }} /></span>
              </button>
            </div>
          })}
        </div>
        {(error ?? refreshError) && !review ? <Text size="caption" tone="secondary">{error ?? refreshError}</Text> : null}
      </div>
    </section>
    {children}
    <Sheet open={current != null} onClose={() => setInspected(null)} width={420}>
      {current ? <SheetPage>
        <ModalHeader title={store.bots.get(current.botId)?.name ?? tr('Member context')} />
        <CardForm>
          <CardSection title={tr('Current context')}>
            <Text size="body">{current.used != null ? `${current.used.toLocaleString()} token` : tr(current.hasSession ? 'Usage not reported' : 'No session yet')}</Text>
            <Text size="footnote" tone="secondary">{tr('Display basis')} · {compact(snapshot?.basis ?? 512000)} · {percentage(current.used, snapshot?.basis ?? 512000) ?? '—'}</Text>
            <Text size="footnote" tone="secondary">{tr('Actual model window')} · {current.size ? compact(current.size) : '—'} · {current.size ? percentage(current.used, current.size) ?? '—' : '—'}</Text>
            {current.shared ? <Text size="footnote" tone="secondary">{tr('This also starts a new private session')}</Text> : null}
          </CardSection>
        </CardForm>
      </SheetPage> : null}
    </Sheet>
    <Sheet open={review != null} onClose={() => setReview(null)} width={530}>
      {review ? <SheetPage>
        <ModalHeader title={tr('Open new session')} trailing={busy ? <Spinner /> : undefined} />
        <CardForm>
          <CardSection title={store.bots.get(review.botId)?.name} footer="The new session starts after the current turn.">
            <Field label="Summary for the new session" value={review.text} onChange={text => setReview({ ...review, text })} lines={[8, 15]} />
          </CardSection>
          {error ? <Text size="footnote">{error}</Text> : null}
          <Button disabled={busy || !review.text.trim()} onClick={() => { void confirm() }}>Confirm new session</Button>
        </CardForm>
      </SheetPage> : null}
    </Sheet>
  </Current.Provider>
}

/** 名字旁只放当前用量，和顶部用同一档位。 */
export function GroupContextChip({ botId }: { botId?: string | null }) {
  const state = useContext(Current)
  const snapshot = state?.snapshot
  const member = snapshot?.members.find(row => row.botId === botId)
  if (!state || !snapshot || !member || member.used == null) return null
  return <button type="button" className="group-context-chip" onClick={() => state.inspect(member.botId)}>
    {compact(member.used)} · {percentage(member.used, snapshot.basis)}
  </button>
}
