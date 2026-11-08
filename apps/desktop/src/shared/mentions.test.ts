import assert from 'node:assert/strict'
import { test } from 'node:test'
import { editMentions, mentionQuery } from './mentions.ts'

// 光标片段使用 JS 原生 UTF-16 偏移，中间插入不能包含尾文。
test('queries follow the caret and ignore nonempty selections and email', () => {
  assert.deepEqual(mentionQuery('😀 @Al trailing', 6), { start: 3, end: 6, query: 'Al' })
  assert.equal(mentionQuery('mail@host', 9), null)
  assert.equal(mentionQuery('@Al tail', 1, 3), null)
  assert.equal(mentionQuery('@A\nmore', 7), null)
})

// 插入片段边界的文本不解除身份；删除或编辑内部则解除。
test('edits preserve outside identities and drop edited tokens', () => {
  const bound = [{ botId: 'same-name-id', start: 2, end: 4 }]
  assert.deepEqual(editMentions('😀@A tail', '😀@A! tail', bound), bound)
  assert.deepEqual(editMentions('😀@A tail', '😀@B tail', bound), [])
  assert.deepEqual(editMentions('😀@A tail', '😀 tail', bound), [])
})

// 即使原生事件只报告折叠光标，删除也不能留下越界或负偏移的身份。
test('collapsed native deletion falls back to the actual changed text', () => {
  const bound = [{ botId: 'a', start: 0, end: 6 }]
  assert.deepEqual(editMentions('@Alice tail', '@Alic tail', bound, { start: 6, end: 6 }), [])
  assert.deepEqual(editMentions('@Alice tail', 'Alice tail', bound, { start: 0, end: 0 }), [])
  assert.deepEqual(editMentions('@Alice tail', '@Aliceb tail', bound, { start: 6, end: 6 }), [])
})

// 扩展平面的汉字也属于名字字符，不能只检查 UTF-16 高代理。
test('supplementary letters extend a token and release its identity', () => {
  const bound = [{ botId: 'a', start: 0, end: 6 }]
  assert.deepEqual(editMentions('@Alice tail', '@Alice𠀀 tail', bound), [])
  assert.deepEqual(editMentions('@Alice tail', '@Alice𠀀 tail', bound, { start: 6, end: 6 }), [])
})
