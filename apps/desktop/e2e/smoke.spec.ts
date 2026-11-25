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
