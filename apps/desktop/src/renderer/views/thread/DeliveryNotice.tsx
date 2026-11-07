import { useState } from 'react'
import type { Entry } from '@shared/models'
import { Button, Disclosure } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { deliveryStatus, deliverySummary } from './delivery-status'

/** 默认只显示一行中文状态；展开仅查看投递元数据，不重复来源消息正文。 */
export function DeliveryNotice({ entry }: { entry: Entry }) {
  const store = useStore()
  const [expanded, setExpanded] = useState(false)
  const rows = entry.data.deliveryRows ?? []
  return <div className="delivery-notice">
    {/* 复用折叠控件的键盘与 aria-expanded 行为，来源内容保留在原消息中。 */}
    <Disclosure expanded={expanded} onToggle={() => setExpanded((value) => !value)} label={<Text size="footnote" tone="secondary" truncate>{deliverySummary(rows)}</Text>}>
      <VStack gap={8} className="fade-in delivery-details">
        <Text size="footnote" tone="secondary">第 {entry.data.deliveryAttempt ?? 1} 次投递{entry.data.parentDeliveryId ? ' · 重试投递' : ''}</Text>
        {rows.map((row) => (
          // 重试仍交给 host 新建投递，折叠仅改变展示，不取消或重放任务。
          <HStack key={row.deliveryId} gap={8}>
            <Text size="footnote" tone="secondary" grow>{store.bots.get(row.targetBotId)?.name ?? '已移除成员'}：{deliveryStatus(row.status)}{row.detail ? ` · ${row.detail}` : ''}</Text>
            {['failed', 'interrupted', 'noReply'].includes(row.status)
              ? <Button kind="secondary" onClick={() => store.retryDelivery(row.deliveryId)}>重新尝试</Button> : null}
          </HStack>
        ))}
      </VStack>
    </Disclosure>
  </div>
}
