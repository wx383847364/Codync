import { useState } from 'react'
import type { DeliveryRow, Entry } from '@shared/models'
import { Button, Disclosure } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { deliveryStatus, deliverySummary } from './delivery-status'

/** 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。 */
export function DeliveryNotice({ entry }: { entry: Entry }) {
  const store = useStore()
  const [expanded, setExpanded] = useState(false)
  const rows = entry.data.deliveryRows ?? []
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
  return <HStack className="delivery-notice" align="flex-start" gap={8}>
    <VStack grow>
      {/* 复用折叠控件的键盘与 aria-expanded 行为，正文保留在原消息中。 */}
      <Disclosure expanded={expanded} onToggle={() => setExpanded(!expanded)} label={<Text size="footnote" tone="secondary" truncate>{deliverySummary(rows)}</Text>}>
        <VStack gap={8} className="fade-in delivery-details">
          <Text size="footnote" tone="secondary">第 {entry.data.deliveryAttempt ?? 1} 次投递{entry.data.parentDeliveryId ? ' · 重试投递' : ''}</Text>
          {rows.map((row) => {
            // 显式引导只作用于该行成员，重试仍由 host 新建投递。
            const id = activeId(row)
            return <HStack key={row.deliveryId} gap={8}>
              <Text size="footnote" tone="secondary" grow>{store.bots.get(row.targetBotId)?.name ?? '已移除成员'}：{deliveryStatus(row.status)}{row.detail ? ` · ${row.detail}` : ''}</Text>
              {id
                ? <Button kind="secondary" onClick={() => store.guideDelivery(row.deliveryId, id)}>引导</Button>
                : ['failed', 'interrupted', 'noReply'].includes(row.status)
                  ? <Button kind="secondary" onClick={() => store.retryDelivery(row.deliveryId)}>重新尝试</Button> : null}
            </HStack>
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
  </HStack>
}
