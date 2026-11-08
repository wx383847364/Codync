import assert from 'node:assert/strict'
import { test } from 'node:test'
import { deliveryStatus, deliverySummary } from './delivery-status.ts'

/** 用目标状态构造展示输入，不依赖来源正文或投递执行逻辑。 */
const row = (status: string, detail = '') => ({ deliveryId: 'd', targetBotId: 'b', status, detail })

/** 保护停止须说明原因，投递次数与整个对话轮数互不混淆。 */
test('a stopped relay has one Chinese explanation without internal status words', () => {
  assert.equal(deliverySummary([row('cancelled', '对话轮次上限')]), '自动接力已达 3 轮，已停止继续派工')
  assert.equal(deliverySummary([row('cancelled', '房间发言上限')]), '本轮发言已达上限，已停止继续派工')
  assert.equal(deliverySummary([row('cancelled', '群派工链深上限')]), '自动接力已达链深上限，已停止继续派工')
})

/** 各终态、排队与执行状态使用中文，新增未知状态有稳定兜底。 */
test('delivery status labels cover every current state and unknown values', () => {
  const expected = ['等待执行', '准备执行', '执行中', '已完成', '失败', '已中断', '未回复', '已停止', '状态待更新']
  assert.deepEqual(['queued', 'claimed', 'started', 'completed', 'failed', 'interrupted', 'noReply', 'cancelled', 'future'].map(deliveryStatus), expected)
  assert.equal(deliverySummary([]), '群内派工状态待更新')
  assert.equal(deliverySummary([row('completed'), row('completed')]), '群内派工已完成')
  assert.equal(deliveryStatus('constructor'), '状态待更新')
  assert.equal(deliverySummary([row('cancelled', 'toString')]), '群内派工已停止')
})

/** 一项达到上限不能隐藏另一项还在执行或排队的事实。 */
test('mixed outcomes retain the active and unsuccessful target counts', () => {
  assert.equal(deliverySummary([row('cancelled', '对话轮次上限'), row('started')]), '群内派工：1 项已停止，1 项执行中')
  assert.equal(deliverySummary([row('completed'), row('queued'), row('queued'), row('failed')]), '群内派工：1 项已完成，2 项等待执行，1 项失败')
})
