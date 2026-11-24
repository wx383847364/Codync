import { useEffect, useState } from 'react'
import { Button, CardForm, CardSection, RowLabel } from '../../components/Controls'
import { Dialog, ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { useStore } from '../../store/context'

type OldSession = { id: string; at: number; state: string; hasMessages: boolean; canDelete: boolean }
type Snapshot = { active: boolean; sessions: OldSession[] }
type Message = { id: string; kind: string; at: number; author?: string; text: string }
const states: Record<string, string> = { archived: 'Archived', retained: 'Retained', deleted: 'Only chat history remains' }

/** Bot 主会话和独立群成员使用同一历史页，Host 确定归属和删除能力。 */
export function SessionHistory({ botId, roomId }: { botId: string; roomId?: string }) {
  const store = useStore()
  const tr = useTr()
  const [open, setOpen] = useState(false)
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [history, setHistory] = useState<Message[] | null>(null)
  const [pending, setPending] = useState<OldSession | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [refresh, setRefresh] = useState(0)
  const online = !store.isOffline && !store.mismatch

  // 展开时读取当前列表，离开后忽略正在返回的列表响应。
  useEffect(() => {
    if (!open || !online) return
    let current = true
    void store.ready().then(client => client.call<Snapshot>('sessionHistory', { roomId, botId })).then(
      value => { if (current) setSnapshot(value) },
      failure => { if (current) setError(failure instanceof Error ? failure.message : String(failure)) },
    )
    return () => { current = false }
  }, [store, botId, roomId, open, online, refresh])

  /** 打开切换时的聊天位置，删除成功后刷新状态并保留聊天入口。 */
  async function act(method: 'sessionHistoryMessages' | 'deleteOldSession', id: string) {
    setBusy(true)
    setError(null)
    try {
      const result = await (await store.ready()).call<{ messages?: Message[] }>(method, { botId, roomId, historyId: id })
      if (method === 'sessionHistoryMessages') setHistory(result.messages ?? [])
      else setRefresh(value => value + 1)
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure))
    } finally {
      setBusy(false)
    }
  }

  /** 关页后清除预览和本次操作反馈。 */
  function close() {
    if (busy) return
    setOpen(false)
    setHistory(null)
    setSnapshot(null)
    setError(null)
  }

  /** 预览中的已载入消息可以定位回原聊天，远端历史仍能直接阅读。 */
  function jump(id: string) {
    const target = document.getElementById(id)
    if (!target) return
    close()
    requestAnimationFrame(() => {
      // 关闭弹层后滚动并恢复原消息焦点。
      target.scrollIntoView({ block: 'center' })
      target.focus({ preventScroll: true })
    })
  }

  return <>
    <Button kind="secondary" disabled={!online} onClick={() => { /* 打开作用域对应的历史列表。 */ setOpen(true) }}>{tr('Session history')}</Button>
    <Sheet open={open} onClose={close}>
      <SheetPage>
        <ModalHeader title={tr(history ? 'Chat history' : 'Session history')} />
        <CardForm>
          {error && <Text role="alert" tone="secondary">{error}</Text>}
          {history ? <>
            <Button kind="secondary" onClick={() => { /* 返回列表，保留当前读取状态。 */ setHistory(null) }}>{tr('Back')}</Button>
            <CardSection>
              {history.length === 0 ? <RowLabel label={tr('No messages')} /> : history.map(message => <VStack key={message.id} gap={4}>
                <Text size="footnote" tone="secondary">{store.bots.get(message.author ?? '')?.name ?? tr(message.kind === 'user' ? 'You' : 'Bot')} · {new Date(message.at).toLocaleString()}</Text>
                <Text style={{ whiteSpace: 'pre-wrap' }}>{message.text}</Text>
                {store.allEntries(roomId ?? botId).some(entry => entry.id === message.id) &&
                  <Button kind="secondary" onClick={() => { /* 跳到这条旧消息的原位。 */ jump(message.id) }}>{tr('Open original message')}</Button>}
              </VStack>)}
            </CardSection>
          </> : <CardSection>
            <RowLabel label={tr(roomId ? 'Current session' : 'Current main session')} detail={tr(snapshot?.active ? 'In use' : 'No session yet')} />
            {snapshot?.sessions.map(item => <VStack key={item.id} gap={6}>
              <RowLabel label={new Date(item.at).toLocaleString()} detail={tr(states[item.state] ?? 'Archive status unknown')} />
              <HStack gap={6}>
                {item.hasMessages && <Button kind="secondary" disabled={busy || !online} onClick={() => { /* 读取历史锚点对应的聊天。 */ void act('sessionHistoryMessages', item.id) }}>{tr('View chat history')}</Button>}
                {item.canDelete && <Button kind="secondary" disabled={busy || !online} onClick={() => { /* 只打开删除确认。 */ setPending(item) }}>{tr('Delete old agent session')}</Button>}
              </HStack>
            </VStack>)}
            {!error && (!snapshot || snapshot.sessions.length === 0) && <RowLabel label={tr(snapshot ? 'No old sessions' : 'Loading…')} />}
          </CardSection>}
        </CardForm>
      </SheetPage>
    </Sheet>
    <Dialog open={pending != null} title="Delete old agent session" message="The original chat history will be kept."
      onClose={() => { /* 取消只清除当前目标。 */ setPending(null) }}
      actions={pending ? [{ title: 'Delete', destructive: true, action: () => {
        // 确认只提交这个历史条目的 ID。
        void act('deleteOldSession', pending.id)
      } }] : []} />
  </>
}
