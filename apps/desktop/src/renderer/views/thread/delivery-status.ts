import type { DeliveryRow } from '@shared/models'

const labels: Record<string, string> = {
  queued: '等待执行', claimed: '准备执行', started: '执行中', completed: '已完成',
  failed: '失败', interrupted: '已中断', noReply: '未回复', cancelled: '已停止',
}
const stopped: Record<string, string> = {
  对话轮次上限: '自动接力已达 3 轮，已停止继续派工',
  群派工链深上限: '自动接力已达链深上限，已停止继续派工',
  房间发言上限: '本轮发言已达上限，已停止继续派工',
}

/** 内部状态只用于选择中文文案，未知状态不暴露内部字段。 */
export function deliveryStatus(status: string): string {
  return Object.hasOwn(labels, status) ? labels[status]! : '状态待更新'
}

/** 聚合当前目标状态；仅全部因同一保护规则停止时，才能宣称本次接力已停止。 */
export function deliverySummary(rows: DeliveryRow[]): string {
  if (!rows.length) return '群内派工状态待更新'
  const first = rows[0]!
  if (Object.hasOwn(stopped, first.detail) && rows.every((row) => row.status === 'cancelled' && row.detail === first.detail)) return stopped[first.detail]!
  const counts = new Map<string, number>()
  for (const row of rows) {
    const label = deliveryStatus(row.status)
    counts.set(label, (counts.get(label) ?? 0) + 1)
  }
  if (counts.size === 1) return `群内派工${deliveryStatus(first.status)}`
  // 混合状态保留数量，避免一项失败或停止掩盖仍在运行的目标。
  return `群内派工：${[...counts].map(([label, count]) => `${count} 项${label}`).join('，')}`
}
