import { test, expect } from '@playwright/test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import type { CodyncBridge } from '../src/shared/ipc.ts'
import { api, attach, typeMessage } from './sandbox.ts'

/** 真实 ACP 回复里的链接打开文件菜单，并经过主进程处理已移走的原文件。 */
test('chat file links open their own menu and report a missing original', async ({}, testInfo) => {
  const { browser, page } = await attach()
  const folder = mkdtempSync(join(tmpdir(), 'codync-file-link-'))
  const bundle = join(folder, 'Example App.app')
  const missing = join(folder, 'Moved page.HTM')
  let botId: string | undefined
  const missingDialog = page.getByRole('alertdialog').filter({ hasText: 'This file could not be found on this computer.' })
  try {
    mkdirSync(bundle)
    writeFileSync(join(folder, 'file-links.md'), [
      `See [\`Example App.app\`](${pathToFileURL(bundle).href}) and [Local HTML page](${pathToFileURL(missing).href}).`,
      'A [web link](https://example.com) and `[example](/tmp/not-a-link.txt)`. ![Inline image](/tmp/not-a-link.png)',
      '/tmp/plain-path.png',
      '~~~md\n[fenced example](/tmp/not-a-link.txt)\n~~~',
    ].join('\n\n'))
    const agent = resolve(__dirname, '../../../host/tests/fixtures/file_agent.py')
    const name = `File links ${Date.now()}`
    botId = (await api<{ bot: { id: string } }>(page, 'createBot', {
      name, backend: 'custom', command: `python3 -X utf8 -u '${agent}'`, cwd: folder, connectors: [], skills: [], notify: false,
    })).bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    await typeMessage(page, 'file-link-menu')
    const bundleLink = page.getByRole('button', { name: 'Example App.app', exact: true })
    await expect(bundleLink).toHaveAttribute('title', process.platform === 'win32' ? bundle.replaceAll('\\', '/') : bundle)
    await expect(page.getByRole('link', { name: 'web link' })).toHaveAttribute('href', 'https://example.com')
    await expect(page.locator('.md-file-link')).toHaveCount(2)

    // 右键文件时不打开消息的复制、回复菜单。
    await bundleLink.click({ button: 'right' })
    await expect(page.getByRole('menuitem', { name: 'Copy path' })).toBeVisible()
    await expect(page.getByRole('menuitem', { name: 'Save as…' })).toBeVisible()
    await expect(page.getByRole('menuitem', { name: process.platform === 'darwin' ? 'Show in Finder' : 'Show in file manager' })).toBeVisible()
    await expect(page.getByRole('menuitem', { name: 'Reply', exact: true })).toHaveCount(0)
    await page.screenshot({ path: testInfo.outputPath('file-link-menu.png'), animations: 'disabled' })
    if (process.platform === 'darwin') {
      // 用键盘切换编辑器页，旧页高亮不会被带入新菜单。
      await page.keyboard.press('Escape')
      await expect(page.getByRole('menu')).toHaveCount(0)
      await bundleLink.focus()
      await page.keyboard.press('Enter')
      await expect(page.getByRole('menuitem', { name: 'Open with…' })).toBeVisible()
      await page.keyboard.press('ArrowDown')
      await page.keyboard.press('ArrowDown')
      await page.keyboard.press('Enter')
      await expect(page.getByRole('menuitem', { name: 'Open with default app' })).toBeVisible()
      await expect(page.locator('.menu-row.highlight')).toHaveCount(0)
    }
    await page.keyboard.press('Escape')
    await expect(page.getByRole('menu')).toHaveCount(0)

    // 右键 HTML 保留文件菜单，普通点击直接发起浏览器操作。
    const html = page.getByRole('button', { name: 'Local HTML page' })
    await html.click({ button: 'right' })
    await expect(page.getByRole('menuitem', { name: 'Open in browser' })).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page.getByRole('menu')).toHaveCount(0)
    await html.click()
    await expect(missingDialog).toBeVisible()
    await expect(page.getByRole('menu')).toHaveCount(0)

    // 其他电脑即使提供当前设备存在的路径也不能请求打开。
    const rejected = await page.evaluate(async (path) => {
      return (window as unknown as { codync: CodyncBridge }).codync.fileLinks.act('another-computer', path, 'open')
    }, bundle)
    expect(rejected).toBe('unsupported')
  } finally {
    // 全套用例共用窗口。关闭本用例打开的提示，避免挡住后续操作。
    if (await missingDialog.isVisible()) await missingDialog.getByRole('button', { name: 'OK' }).click()
    // 只删除此用例创建的沙箱机器人及临时文件。
    if (botId) await api(page, 'deleteBot', { botId }).catch(() => {})
    await browser.close()
    rmSync(folder, { force: true, recursive: true })
  }
})
