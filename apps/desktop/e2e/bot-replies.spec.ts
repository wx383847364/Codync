import { test, expect } from '@playwright/test'
import { mkdtempSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { api, attach, typeMessage } from './sandbox.ts'

type Entry = { kind: string; data: { text?: string; final?: boolean; status?: string; delegationId?: string } }

/** 从真实沙箱的聊天记录观察完成状态，避免把流式文本误认为完成交付。 */
async function history(page: Parameters<typeof api>[0], botId: string) {
  return (await api<{ entries: Entry[] }>(page, 'history', { botId })).entries
}

// 真实桌面验证长回复保留正文传递，并由假执行器经过真实 MCP 再取回中间证据。
test('a bot receives a long local reply and can retrieve its middle', async () => {
  const { browser, page } = await attach()
  const folder = mkdtempSync(join(tmpdir(), 'codync-reply-desktop-'))
  const bots: string[] = []
  const name = `Reply e2e ${Date.now()}`
  const agent = resolve(__dirname, '../../../host/tests/fixtures/team_agent.py')
  try {
    for (const role of ['lead', 'reviewer']) {
      const cwd = join(folder, role)
      mkdirSync(cwd)
      const { bot } = await api<{ bot: { id: string } }>(page, 'createBot', {
        name: `${name} ${role}`, backend: 'custom', command: `python3 -X utf8 -u '${agent}'`,
        cwd, connectors: [], skills: [], notify: false,
      })
      bots.push(bot.id)
    }
    const [lead, reviewer] = bots
    await page.locator('.roster-row', { hasText: `${name} lead` }).click()
    await typeMessage(page, `DELEGATE ${reviewer} LONG_REPLY`)
    // 等待通知完成与主脑处理正文，验证撤回预览后关键证据无需再次读取。
    await expect.poll(async () => (await history(page, lead)).some((e) => e.data.status === 'completed' && e.data.delegationId !== undefined)).toBe(true)
    await expect(page.locator('.transcript')).toContainText('中间证据')
    await expect(page.locator('.transcript')).not.toContainText('middle omitted')
    const entries = await history(page, lead)
    const request = entries.find((e) => e.data.delegationId)?.data.delegationId
    expect(request).toBeTruthy()
    const offset = '结论：已完成\n'.length + 4400
    await typeMessage(page, `READ_REPLY ${JSON.stringify({ requestId: request, offset, limit: 4 })}`)
    // 此文本必须来自模型可见的 MCP 返回，而非直接查询 SQLite。
    await expect.poll(async () => {
      const result = (await history(page, lead)).find((e) => e.data.final && e.data.text?.includes('totalCharacters'))
      return result?.data.text ? JSON.parse(result.data.text).text : null
    }).toBe('中间证据')
    await expect(page.locator('.transcript')).toContainText('中间证据')
  } finally {
    for (const botId of bots) {
      // 删除本测试创建的 bot 会停止其进程，不触碰共享沙箱或其他任务。
      await api(page, 'deleteBot', { botId })
    }
    rmSync(folder, { recursive: true, force: true })
    await browser.close()
  }
})
