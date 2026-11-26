import { test, expect } from '@playwright/test'
import type { HostSnapshot } from '../src/shared/ipc.ts'
import { execFileSync } from 'node:child_process'
import { attach, openGeneralSettings, sandbox } from './sandbox.ts'

test('the sandbox app renders and runs against its own host', async () => {
  const { browser, page } = await attach()
  try {
    await expect(page.locator('#root')).not.toBeEmpty()
    const host = await page.evaluate(
      () => (window as unknown as { codync: { host: { snapshot(): Promise<HostSnapshot> } } }).codync.host.snapshot(),
    )
    // The sandbox host, never Codync Dev's shared one on 19223.
    expect(host.state.kind).toBe('running')
    expect(host.dev).toBe(true)
    expect(host.local?.baseURL).toBe(`http://127.0.0.1:${sandbox().port}`)
  } finally {
    await browser.close()
  }
})


// 验证构建提交号从主进程进入更新页，方便区分相同版本号的包。
test('updates identifies the running build by commit', async () => {
  const { browser, page } = await attach()
  try {
    const commit = execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], { encoding: 'utf8' }).trim()
    await openGeneralSettings(page)
    await page.getByRole('button', { name: 'Updates', exact: true }).click()
    await expect(page.getByText('Build commit', { exact: true })).toBeVisible()
    await expect(page.locator('code').filter({ hasText: commit })).toBeVisible()
  } finally {
    await browser.close()
  }
})

// 扩大的 macOS 标题带覆盖侧栏空白，但搜索按钮仍可点击。
test('the macOS sidebar title band preserves its search control', async () => {
  test.skip(process.platform !== 'darwin', 'macOS custom title bar')
  const { browser, page } = await attach()
  try {
    // 此套件上一项打开「更新」设置，先收起弹层以测试标题带自身的命中区域。
    await page.keyboard.press('Escape')
    await expect(page.locator('.modal-card.shown')).toHaveCount(0)
    const header = page.locator('.sidebar:not(.compact) .sidebar-header')
    await expect(header).toBeVisible()
    const box = await header.boundingBox()
    expect(box?.height).toBe(64)
    // 从交通灯右侧、工具按钮左侧验证接近下缘的新增空白仍属于标题带。
    const hitsTitleBand = await header.evaluate((element) => {
      const { left, top } = element.getBoundingClientRect()
      const hit = document.elementFromPoint(left + 100, top + 59)
      return hit?.closest('.sidebar-header') === element && !hit.closest('.no-drag')
    })
    expect(hitsTitleBand).toBe(true)
    await header.getByRole('button', { name: 'Search', exact: true }).click()
    await expect(page.getByRole('textbox', { name: 'Search bots' })).toBeVisible()
    await page.keyboard.press('Escape')
  } finally {
    await browser.close()
  }
})
