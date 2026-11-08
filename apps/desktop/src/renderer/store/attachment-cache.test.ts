import assert from 'node:assert/strict'
import { test } from 'node:test'
import { AttachmentCache } from './attachment-cache.ts'

/** 多图同时出现时，最多两个实际下载，同一文件共享读取结果。 */
test('image downloads share requests and keep concurrency bounded', async () => {
  const attachmentCache = new AttachmentCache()
  let active = 0
  let peak = 0
  let calls = 0
  const completions: (() => void)[] = []
  // 下载完成由测试控制，观察排队与槽位移交的实际行为。
  const fetch = async () => {
    calls++
    active++
    peak = Math.max(peak, active)
    await new Promise<void>((resolve) => { /* 测试控制下载完成，观察排队。 */ completions.push(resolve) })
    active--
    return new Uint8Array([calls])
  }
  const first = attachmentCache.read('one', fetch)
  const duplicate = attachmentCache.read('one', fetch)
  const rest = ['two', 'three', 'four'].map((id) => { /* 多个文件争用相同下载槽。 */ return attachmentCache.read(id, fetch) })
  await new Promise<void>((resolve) => { /* 让出一个事件周期，等待槽位交接。 */ setImmediate(resolve) })
  assert.equal(calls, 2)
  while (completions.length) {
    completions.shift()!()
    await new Promise<void>((resolve) => { /* 让出一个事件周期，等待槽位交接。 */ setImmediate(resolve) })
  }
  const [one, same] = await Promise.all([first, duplicate, ...rest])
  assert.strictEqual(one, same)
  assert.equal(calls, 4)
  assert.equal(peak, 2)
})

/** 缓存超过 64 MiB 时淘汰旧图片，可在之后重新读取。 */
test('image bytes evict the oldest data at the memory limit', async () => {
  const attachmentCache = new AttachmentCache()
  attachmentCache.put('old', new Uint8Array(40 * 1024 * 1024))
  attachmentCache.put('new', new Uint8Array(40 * 1024 * 1024))
  let reads = 0
  await attachmentCache.read('old', async () => { /* 被淘汰的数据必须重新读取。 */ reads++; return new Uint8Array([1]) })
  await attachmentCache.read('new', async () => { /* 缓存命中时不应调用此回调。 */ reads++; return null })
  assert.equal(reads, 1)
})

/** 退场会释放缓存，晚到下载和已排队读取都不能再次访问旧账号数据。 */
test('retiring a cache drops bytes and skips queued reads', async () => {
  const cache = new AttachmentCache()
  let complete: (() => void) | undefined
  let calls = 0
  const first = cache.read('one', async () => { /* 保持一个下载在途，制造退场竞态。 */ calls++; await new Promise<void>((resolve) => { /* 留存完成信号。 */ complete = resolve }); return new Uint8Array([1]) })
  const second = cache.read('two', async () => { /* 第二个槽位已有任务。 */ return new Uint8Array([2]) })
  const third = cache.read('three', async () => { /* 退场后排队任务不得开始。 */ calls++; return new Uint8Array([3]) })
  cache.retire()
  complete?.()
  assert.deepEqual(await Promise.all([first, second, third]), [null, null, null])
  assert.equal(calls, 1)
  assert.equal(await cache.read('one', async () => { /* 退场后不能再调用读取器。 */ throw new Error('must not fetch after retirement') }), null)
})
