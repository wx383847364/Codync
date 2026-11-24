import { test, expect } from '@playwright/test'
import { writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { api, attach } from './sandbox.ts'

/** 使用真实 Host、托管工作区和 ACP fixture 验证三处入口以及删除确认。 */
test('member activity, managed disk usage, and old session deletion', async () => {
  const { browser, page } = await attach()
  const ids: string[] = []
  const tag = Date.now()
  try {
    const name = `HistoryBot${tag}`
    const { bot } = await api<{ bot: { id: string; cwd: string } }>(page, 'createBot', {
      name, backend: 'custom', connectors: [], skills: [], notify: false,
      command: `python3 -u '${join(__dirname, '../../../host/tests/fixtures/team_agent.py')}'`,
    })
    ids.push(bot.id)
    writeFileSync(join(bot.cwd, 'file.bin'), Buffer.alloc(16 * 4096))
    writeFileSync(join(bot.cwd, 'session-retirement.json'), '{"nativeArchive":true}')
    writeFileSync(join(bot.cwd, 'group-script.json'), JSON.stringify({
      reply: 'An original group result', activityTool: true, activityDelay: 1.5,
    }))
    const roomName = `History Room ${tag}`
    const { bot: room } = await api<{ bot: { id: string } }>(page, 'createBot', { name: roomName, kind: 'group', members: [bot.id] })
    ids.unshift(room.id)
    await page.locator('.roster-row', { hasText: roomName }).click()
    await api(page, 'send', { botId: room.id, text: 'Run the activity fixture.' })
    const dispatch = page.locator('.delivery-notice').first()
    await dispatch.getByRole('button', { name: /群内派工/ }).click()
    await expect(dispatch.getByText(/Running a command · (In progress|Step completed)/)).toBeVisible()
    await expect(dispatch.getByText('PRIVATE_TOOL_TITLE')).toHaveCount(0)
    await expect(dispatch.getByText(/Running a command · Step completed/)).toBeVisible()
    await dispatch.getByRole('button', { name: 'View result' }).click()
    await expect(page.getByRole('log').getByText('An original group result')).toBeVisible()

    await page.locator('.title-pill').click()
    await page.locator('.group-context-reading').click()
    await expect(page.getByText('Member activity', { exact: true })).toBeVisible()
    await expect(page.getByRole('dialog').getByText(/Running a command · Step completed/)).toBeVisible()
    await page.getByRole('button', { name: 'Open original assignment' }).click()
    await expect(dispatch.getByText(/Running a command · Step completed/)).toBeVisible()

    const groupUsage = await api<{ members: { bytes: number }[] }>(page, 'groupWorkspaceUsage', { roomId: room.id })
    const ownUsage = await api<{ bytes: number; directory: string }>(page, 'botWorkspaceUsage', { botId: bot.id })
    expect(ownUsage.directory).toBe(bot.cwd)
    expect(ownUsage.bytes).toBeGreaterThanOrEqual(65536)
    expect(ownUsage.bytes).toBe(groupUsage.members[0]?.bytes)
    await page.getByRole('button', { name: `${name}, Custom agent`, exact: true }).click()
    if (!(await page.getByText('Disk usage', { exact: true }).isVisible())) await page.locator('.title-pill').click()
    const disk = page.locator('.details-group-usage-card', { hasText: 'Disk usage' })
    await expect(disk.getByText('Managed workspace')).toBeVisible()
    await expect(disk.getByRole('button', { name: 'Open folder' })).toBeEnabled()

    await api(page, 'send', { botId: bot.id, text: 'Keep this private chat.' })
    await expect(page.getByRole('log').getByText('reply: Keep this private chat.')).toBeVisible()
    await api(page, 'newSession', { botId: bot.id, mode: 'after' })
    await expect.poll(async () => {
      const history = await api<{ sessions: { state: string }[] }>(page, 'sessionHistory', { botId: bot.id })
      return history.sessions[0]?.state
    }).toBe('archived')
    await page.getByRole('button', { name: 'Session history' }).click()
    const history = page.getByRole('dialog', { name: 'Session history' })
    await expect(history.getByText('Archived', { exact: true })).toBeVisible()
    await history.getByRole('button', { name: 'Delete old agent session' }).click()
    let confirm = page.getByRole('alertdialog', { name: 'Delete old agent session' })
    await expect(confirm.getByText('The original chat history will be kept.')).toBeVisible()
    await confirm.getByRole('button', { name: 'Cancel' }).click()
    await expect(history.getByText('Archived', { exact: true })).toBeVisible()
    // 删除失败仍保留条目和聊天入口，后续操作继续需要确认。
    writeFileSync(join(bot.cwd, 'session-retirement.json'), '{"nativeArchive":true,"deleteFails":true}')
    await history.getByRole('button', { name: 'Delete old agent session' }).click()
    await page.getByRole('alertdialog', { name: 'Delete old agent session' }).getByRole('button', { name: 'Delete', exact: true }).click()
    await expect(history.getByRole('alert')).toContainText('delete failed')
    await expect(history.getByText('Archived', { exact: true })).toBeVisible()
    writeFileSync(join(bot.cwd, 'session-retirement.json'), '{"nativeArchive":true}')
    await history.getByRole('button', { name: 'Delete old agent session' }).click()
    confirm = page.getByRole('alertdialog', { name: 'Delete old agent session' })
    await confirm.getByRole('button', { name: 'Delete', exact: true }).click()
    await expect(history.getByText('Only chat history remains')).toBeVisible()
    await history.getByRole('button', { name: 'View chat history' }).click()
    await expect(page.getByRole('dialog', { name: 'Chat history' }).getByText('reply: Keep this private chat.')).toBeVisible()
  } finally {
    await page.keyboard.press('Escape').catch(() => {})
    for (const botId of ids) await api(page, 'deleteBot', { botId }).catch(() => {})
    await browser.close()
  }
})
