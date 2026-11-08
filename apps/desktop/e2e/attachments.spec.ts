import { test, expect, type Page } from '@playwright/test'
import { mkdirSync, mkdtempSync, writeFileSync, rmSync, unlinkSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { api, attach, steerBot, typeMessage } from './sandbox.ts'

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

/** 私聊与当前群的显式文件均保留原图；正文路径不触发读取，适应窗口仍为默认。 */
test('explicit images keep original pixels in personal and group chats without scanning text paths', async () => {
  const { browser, page } = await attach()
  const folder = mkdtempSync(join(tmpdir(), 'codync-explicit-images-'))
  let bot: string | undefined
  let group: string | undefined
  const name = `Original files ${Date.now()}`
  try {
    const image = await page.evaluate(() => {
      // 原生编码生成大于旧下采样上限的图，验证实际像素而非一张被拉伸的缩略图。
      const canvas = document.createElement('canvas'); canvas.width = 6000; canvas.height = 128
      const drawing = canvas.getContext('2d')!; drawing.fillStyle = '#235080'; drawing.fillRect(0, 0, 6000, 128)
      return canvas.toDataURL('image/png').split(',')[1]!
    })
    writeFileSync(join(folder, 'image.png'), Buffer.from(image, 'base64'))
    const agent = resolve(__dirname, '../../../host/tests/fixtures/file_agent.py')
    bot = (await api<{ bot: { id: string } }>(page, 'createBot', {
      name, backend: 'custom', command: `python3 -X utf8 -u '${agent}'`, cwd: folder, connectors: [], skills: [], notify: false,
    })).bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    await typeMessage(page, `path-text-only ${join(folder, 'image.png')}`)
    await expect(page.locator('.transcript')).toContainText('The files are ready.')
    await expect(page.locator('.file-card-image')).toHaveCount(0)
    await typeMessage(page, 'share-image')
    const picture = page.locator('.file-card-image img')
    await expect(picture).toHaveCount(1)
    await picture.click()
    const shown = page.locator('.attachment-viewer img')
    await expect(shown).toBeVisible()
    await page.getByRole('button', { name: '100%', exact: true }).click()
    await expect.poll(() => shown.evaluate((node) => node.getBoundingClientRect().width)).toBe(6000)
    expect(await shown.evaluate((node) => (node as HTMLImageElement).naturalWidth)).toBe(6000)
    await page.getByRole('button', { name: 'Fit to window', exact: true }).click()
    await expect.poll(async () => (await shown.boundingBox())!.width).toBeLessThan(1040)
    await page.keyboard.press('Escape')
    group = (await api<{ bot: { id: string } }>(page, 'createBot', { name: `${name} group`, kind: 'group', members: [bot], notify: false })).bot.id
    await page.locator('.roster-row', { hasText: `${name} group` }).click()
    await typeMessage(page, 'share-image')
    await expect(picture).toHaveCount(1)
    // 删除原文件并重载，要求从群自己的不可变快照重新取回。
    unlinkSync(join(folder, 'image.png'))
    await page.reload()
    await page.locator('.roster-row', { hasText: `${name} group` }).click()
    await expect(picture).toHaveCount(1)
    await picture.click()
    await page.getByRole('button', { name: '100%', exact: true }).click()
    await expect.poll(() => shown.evaluate((node) => node.getBoundingClientRect().width)).toBe(6000)
    const evidence = join(tmpdir(), 'codync-attachment-e2e'); mkdirSync(evidence, { recursive: true })
    await page.screenshot({ path: join(evidence, 'group-original-image.png') })
    await page.keyboard.press('Escape')
  } finally {
    // 仅清理该用例创建的群和执行器，用户任务与其他沙箱保持运行。
    if (group) await api(page, 'deleteBot', { botId: group }).catch(() => {})
    if (bot) await api(page, 'deleteBot', { botId: bot }).catch(() => {})
    rmSync(folder, { recursive: true, force: true })
    await browser.close()
  }
})

/** 真实图片组件从离线失败恢复到图片，不让旧失败标记遮住已取回的内容。 */
test('an image preview recovers after an offline cache miss', async () => {
  const { browser, page } = await attach()
  try {
    await page.evaluate(async () => {
      // 使用此窗口实际加载的 React，避免测试创建第二份 hook 运行时。
      const resources = performance.getEntriesByType('resource').map((item) => item.name)
      const reactURL = resources.find((url) => /\/react\.js[?]/.test(url))!
      const domURL = resources.find((url) => /\/react-dom_client\.js[?]/.test(url))!
      const reactModule = await import(reactURL)
      const React = reactModule.default ?? reactModule
      const domModule = await import(domURL)
      const { createRoot } = domModule.default ?? domModule
      // 地址由当前 Vite 沙箱提供，在浏览器中解析，不交给 Node 的模块解析器。
      const contextURL = resources.find((url) => /\/store\/context\.ts(?:[?]|$)/.test(url))
        ?? new URL('/store/context.ts', location.origin).href
      const componentURL = resources.find((url) => /\/views\/thread\/Attachments\.tsx(?:[?]|$)/.test(url))
        ?? new URL('../views/thread/Attachments.tsx', contextURL).href
      const { StoreContext } = await import(contextURL)
      const { AttachmentImage } = await import(componentURL)
      const listeners = new Set<() => void>()
      const canvas = document.createElement('canvas'); canvas.width = 16; canvas.height = 16
      const bytes = Uint8Array.from(atob(canvas.toDataURL('image/png').split(',')[1]!), (value) => value.charCodeAt(0))
      const store = {
        connection: { kind: 'offline' },
        // 网络响应由用例控制，只模拟该组件的连接生命周期，不触碰实际 Host。
        attachmentData: async () => store.connection.kind === 'online' ? bytes : null,
        subscribe: (changed: () => void) => { listeners.add(changed); return () => listeners.delete(changed) },
      }
      const holder = document.createElement('div') as HTMLDivElement & { dispose?: () => void }
      holder.id = 'image-reconnect-probe'
      holder.style.cssText = 'position:fixed;inset:0 auto auto 0;z-index:100000;padding:20px;background:white'
      document.body.append(holder)
      const root = createRoot(holder)
      root.render(React.createElement(StoreContext.Provider, { value: store },
        React.createElement(AttachmentImage, { source: { file: { id: 'probe', name: 'probe.png', size: bytes.length }, botId: 'probe' }, fit: 'thumbnail' })))
      // 仅测试自建组件接收上线通知；生产 Store 和用户任务不变。
      const online = () => { store.connection = { kind: 'online' }; for (const changed of listeners) changed() }
      window.addEventListener('codync-test-image-online', online)
      holder.dispose = () => { root.unmount(); window.removeEventListener('codync-test-image-online', online); holder.remove() }
    })
    const probe = page.locator('#image-reconnect-probe')
    await expect(probe.getByRole('button', { name: 'Retry image', exact: true })).toBeVisible()
    await page.evaluate(() => window.dispatchEvent(new Event('codync-test-image-online')))
    await expect(probe.locator('img')).toBeVisible()
    await expect(probe.getByRole('button', { name: 'Retry image', exact: true })).toHaveCount(0)
  } finally {
    // 组件回归的探针仅存在于此测试页面，结束时立即卸载。
    await page.evaluate(() => (document.getElementById('image-reconnect-probe') as (HTMLElement & { dispose?: () => void }) | null)?.dispose?.())
    await browser.close()
  }
})
