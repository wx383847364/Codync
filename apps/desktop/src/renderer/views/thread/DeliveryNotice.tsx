import type { Entry } from '@shared/models'
import { Button } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'

/** 来源事件与 attempt 聚合显示；保留原文名字，每个目标显示当前成员名和可查询状态。 */
export function DeliveryNotice({ entry }: { entry: Entry }) {
  const store = useStore()
  return <VStack gap={8}>
    <Text size="footnote" tone="secondary">Attempt {entry.data.deliveryAttempt ?? 1}{entry.data.parentDeliveryId ? ` · retry of ${entry.data.parentDeliveryId.slice(0, 8)}` : ''}</Text>
    <Text size="footnote" tone="secondary">{entry.data.deliveryBody ?? ''}</Text>
    {entry.data.deliveryRows?.map((row) => (
      // 重试只发送意图，新的投递及 attempt 由 host 创建。
      <HStack key={row.deliveryId} gap={8}>
        <Text size="footnote" tone="secondary">{store.bots.get(row.targetBotId)?.name ?? row.targetBotId}: {row.status} {row.detail}</Text>
        {['failed', 'interrupted', 'noReply'].includes(row.status)
          ? <Button kind="secondary" onClick={() => store.retryDelivery(row.deliveryId)}>Try again</Button> : null}
      </HStack>
    ))}
  </VStack>
}
