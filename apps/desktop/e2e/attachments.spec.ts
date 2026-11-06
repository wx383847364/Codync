import { test, expect, type Page } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { api, attach, steerBot } from './sandbox.ts'

/** 在浏览器内构造真实文件拖放，经过 Composer 的读取和接收流程。 */
async function drop(page: Page, kind: 'image' | 'video' | 'broken') {
  const transfer = await page.evaluateHandle(async (kind) => {
    const files = new DataTransfer()
    if (kind === 'image') {
      const canvas = document.createElement('canvas')
      canvas.width = 640; canvas.height = 480
      const context = canvas.getContext('2d')!
      context.fillStyle = '#40a070'; context.fillRect(0, 0, 640, 480)
      const blob = await new Promise<Blob>((resolve) => canvas.toBlob((blob) => resolve(blob!), 'image/png'))
      files.items.add(new File([blob], 'draft.png', { type: 'image/png' }))
    } else {
      // 上传只传递二进制；MP4 扩展用于覆盖视频选取路径，不测试视频播放或转码。
      const bytes = new Uint8Array(kind === 'video' ? 101 * 1024 * 1024 : 3)
      bytes[bytes.length - 1] = 42
      files.items.add(new File([bytes], kind === 'video' ? 'large-video.mp4' : 'broken.png'))
    }
    return files
  }, kind)
  try { await page.locator('.composer').dispatchEvent('drop', { dataTransfer: transfer }) }
  finally { await transfer.dispose() }
}

/** 双击及键盘打开本地大图，关闭后草稿和附件仍在，移除不误开预览或发送。 */
test('draft image preview opens on double click and keyboard without sending', async () => {
  const { browser, page } = await attach()
  let bot: string | undefined
  try {
    const name = `Draft preview ${Date.now()}`
    bot = (await steerBot(page, name, 1)).id
    await page.locator('.roster-row', { hasText: name }).click()
    const field = page.locator('.composer textarea')
    await field.fill('keep this draft')
    await drop(page, 'image')
    const preview = page.getByRole('button', { name: 'Preview draft.png', exact: true })
    await expect(preview).toBeVisible()
    await preview.click()
    await expect(page.locator('.attachment-viewer')).toHaveCount(0)
    await preview.dblclick()
    await expect(page.locator('.attachment-viewer img')).toBeVisible()
    // 等面板淡入完成，避免仅有布局但仍透明的节点被当作可见大图。
    await expect(page.locator('.modal-content')).toHaveCSS('opacity', '1')
    // 原图为 640×480，必须明显大于 80px 的输入框缩略图且不裁剪。
    const bounds = await page.locator('.attachment-viewer img').boundingBox()
    expect(bounds!.width).toBeGreaterThan(80)
    const evidence = join(tmpdir(), 'codync-attachment-e2e')
    mkdirSync(evidence, { recursive: true })
    await page.screenshot({ path: join(evidence, 'draft-image-preview.png') })
    await page.keyboard.press('Escape')
    await expect(page.locator('.attachment-viewer')).toHaveCount(0)
    await expect(field).toHaveValue('keep this draft')
    await expect(preview).toBeVisible()
    for (const key of ['Enter', 'Space']) {
      await preview.focus()
      await page.keyboard.press(key)
      await expect(page.locator('.attachment-viewer img')).toBeVisible()
      await expect(page.locator('.modal-content')).toHaveCSS('opacity', '1')
      await page.keyboard.press('Escape')
    }
    await page.getByRole('button', { name: 'Remove draft.png', exact: true }).click()
    await expect(preview).toHaveCount(0)
    await expect(page.locator('.attachment-viewer')).toHaveCount(0)
    await drop(page, 'broken')
    await expect(page.locator('.file-chip')).toContainText('broken.png')
    await expect(page.getByRole('button', { name: 'Preview broken.png', exact: true })).toHaveCount(0)
    const history = await api<{ entries: { kind: string }[] }>(page, 'history', { botId: bot })
    expect(history.entries.filter((entry) => entry.kind === 'user')).toHaveLength(0)
  } finally {
    // 仅清理本用例的沙盒 bot，保留失败断言的原始原因。
    if (bot) await api(page, 'deleteBot', { botId: bot }).catch(() => {})
    await browser.close()
  }
})

/** 从真实输入框发送超过旧上限的视频附件，并从 host 读回末字节确认未截断。 */
test('a video attachment larger than 100 MiB uploads and sends completely', async () => {
  test.setTimeout(120_000)
  const { browser, page } = await attach()
  let bot: string | undefined
  try {
    const name = `Large attachment ${Date.now()}`
    bot = (await steerBot(page, name, 1)).id
    await page.locator('.roster-row', { hasText: name }).click()
    await drop(page, 'video')
    await expect(page.locator('.file-chip')).toContainText('large-video.mp4')
    await page.locator('.composer textarea').fill('large attachment acceptance')
    await page.locator('.composer').getByRole('button', { name: 'Send', exact: true }).click()
    type File = { id: string; name: string; size: number }
    type History = { entries: { kind: string; data: { attachments?: File[] } }[] }
    // 只读取已落盘历史，不把乐观显示的本地发送行算作 host 受理。
    await expect.poll(async () => (await api<History>(page, 'history', { botId: bot })).entries.find((entry) => entry.kind === 'user')?.data.attachments?.[0]?.size, { timeout: 90_000 }).toBe(101 * 1024 * 1024)
    const entry = (await api<History>(page, 'history', { botId: bot })).entries.find((entry) => entry.kind === 'user')!
    const file = entry.data.attachments![0]!
    const tail = await api<{ data: string; size: number }>(page, 'readUpload', { botId: bot, uploadId: file.id, offset: file.size - 1 })
    expect(tail.size).toBe(file.size)
    expect(Buffer.from(tail.data, 'base64')).toEqual(Buffer.from([42]))
    const evidence = join(tmpdir(), 'codync-attachment-e2e')
    mkdirSync(evidence, { recursive: true })
    await page.screenshot({ path: join(evidence, 'large-video-uploaded.png') })
  } finally {
    if (bot) await api(page, 'deleteBot', { botId: bot }).catch(() => {})
    await browser.close()
  }
})
