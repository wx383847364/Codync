import { test, expect, type Page } from '@playwright/test'
import { resolve } from 'node:path'
import { canSteer } from '../src/renderer/views/thread/queued-messages.ts'
import { api, attach, steerBot, typeMessage } from './sandbox.ts'

type History = { entries: { kind: string; data: { text?: string; status?: string } }[] }

/** Whether the turn answering `text` has started. */
async function started(page: Page, botId: string, text: string): Promise<boolean> {
  const { entries } = await api<History>(page, 'history', { botId })
  return entries.some((e) => e.kind === 'user' && e.data.text === text && e.data.status === 'sent')
}

test('a long queue scrolls, with its oldest message in reach', async () => {
  const { browser, page } = await attach()
  const name = `Long queue e2e ${Date.now()}`
  let botId: string | undefined
  try {
    const bot = await steerBot(page, name, 120)
    botId = bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    await api(page, 'send', { botId, text: 'first' })
    await expect.poll(() => started(page, bot.id, 'first')).toBe(true)
    for (let i = 1; i <= 40; i++) await api(page, 'send', { botId, text: `Waiting ${i}` })
    const queued = page.locator('.queued-row')
    await expect(queued).toHaveCount(40)

    await expect(page.locator('.composer textarea')).toBeInViewport()
    const list = page.locator('.queued-list')
    const box = await list.boundingBox()
    expect(box!.y).toBeGreaterThan(0)
    expect(box!.height).toBeLessThan(260)
    const row = (text: string) => queued.filter({ hasText: new RegExp(`^${text}$`) })
    await expect(row('Waiting 1')).toBeInViewport()
    await expect(row('Waiting 40')).not.toBeInViewport()

    await list.hover()
    await page.mouse.wheel(0, 2000)
    await expect(row('Waiting 40')).toBeInViewport()
    await row('Waiting 40').getByRole('button', { name: 'Remove' }).click()
    await expect(queued).toHaveCount(39)
    await page.mouse.wheel(0, -2000)
    await expect(row('Waiting 1')).toBeInViewport()

    // One typed here scrolls into view; the ones the computer got from elsewhere didn't.
    await typeMessage(page, 'typed here')
    await expect(row('typed here')).toBeInViewport()
  } finally {
    if (botId) {
      await api(page, 'stop', { botId }).catch(() => {})
      await api(page, 'deleteBot', { botId }).catch(() => {})
    }
    await browser.close()
  }
})

test('a message sent while the bot works waits above the composer, then goes out now or is taken back', async () => {
  const { browser, page } = await attach()
  const name = `Queue e2e ${Date.now()}`
  let botId: string | undefined
  try {
    const { version } = await api<{ version: string }>(page, 'hello', {})
    test.skip(!canSteer(version), `the sandbox host (${version}) can't steer yet`)
    const bot = await steerBot(page, name, 30)
    botId = bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    const transcript = page.locator('.transcript')
    const queued = page.locator('.queued-row')

    await api(page, 'send', { botId, text: 'first' })
    await expect.poll(() => started(page, bot.id, 'first')).toBe(true)
    await typeMessage(page, 'second')
    await expect(queued.filter({ hasText: 'second' })).toBeVisible()
    await expect(transcript).not.toContainText('second')
    await queued.filter({ hasText: 'second' }).getByRole('button', { name: 'Send now' }).click()
    // It joined the running turn: the chat shows it, and the reply took it in.
    await expect(queued).toHaveCount(0)
    await expect(transcript).toContainText('second')
    await expect(transcript).toContainText('Got: ')

    await api(page, 'send', { botId, text: 'third' })
    await expect.poll(() => started(page, bot.id, 'third')).toBe(true)
    await typeMessage(page, 'never sent')
    await queued.filter({ hasText: 'never sent' }).getByRole('button', { name: 'Remove' }).click()
    await expect(queued).toHaveCount(0)
    await expect(transcript).not.toContainText('never sent')
    const { entries } = await api<History>(page, 'history', { botId })
    expect(entries.find((e) => e.data.text === 'never sent')).toBeUndefined()
  } finally {
    if (botId) {
      await api(page, 'stop', { botId }).catch(() => {})
      await api(page, 'deleteBot', { botId }).catch(() => {})
    }
    await browser.close()
  }
})

/** 在真实桌面应用验证机器人详情的实报指标和容量交接操作。 */
test('bot details show reported usage and the handoff continues the queued task', async () => {
  const { browser, page } = await attach()
  const agent = resolve(__dirname, '../../../host/tests/fixtures/instructions_agent.py')
  const name = `Session usage e2e ${Date.now()}`
  let botId: string | undefined
  try {
    const { bot } = await api<{ bot: { id: string } }>(page, 'createBot', {
      name, backend: 'custom', command: `python3 -X utf8 -u "${agent}" first-resume`, connectors: [], skills: [], notify: false,
    })
    botId = bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    await typeMessage(page, 'USAGE 12000 SIZE 18000 REPORT_TOKENS')
    await expect(page.locator('.transcript').getByText('ok', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: `${name}, view details`, exact: true }).click()
    const details = page.locator('.details-panel')
    await expect(details.getByText('Main session context')).toBeVisible()
    await expect(details.getByText('12,000 / 18,000')).toBeVisible()
    await details.getByRole('button', { name: /custom · Unknown model/ }).click()
    await expect(details.getByText('Input tokens · 1,234')).toBeVisible()
    await expect(details.getByText('Cache read tokens · 0')).toBeVisible()
    await expect(details.getByText('Cache write tokens · Unavailable')).toBeVisible()
    await details.getByRole('button', { name: 'Close details' }).click()

    const task = 'Continue the original work. ' + 'provide a clear response, include references, and review changes. '.repeat(550).trim()
    await api(page, 'send', { botId, text: task })
    const start = page.getByRole('button', { name: 'Start new session with summary' })
    await expect(start).toBeVisible()
    expect(await started(page, botId, task)).toBe(false)
    await start.click()
    await expect.poll(() => started(page, bot.id, task)).toBe(true)
    await expect(page.locator('.transcript').getByText('ok', { exact: true })).toHaveCount(2)
    await expect(page.locator('.roster-row', { hasText: name })).toHaveCount(1)
    await page.getByRole('button', { name: `${name}, view details`, exact: true }).click()
    await details.getByRole('button', { name: 'Prepare new session with summary' }).click()
    await expect(details).toBeHidden()
    await expect(page.getByText('Your next message starts a fresh session carrying this summary.')).toBeVisible()
    await typeMessage(page, 'a new private task')
    await expect(page.locator('.transcript').getByText('ok', { exact: true })).toHaveCount(3)
    await page.getByRole('button', { name: `${name}, view details`, exact: true }).click()
    await expect(details.getByRole('button', { name: 'Prepare new session with summary' })).toBeVisible()
    await api(page, 'updateBot', { id: botId, command: `python3 -X utf8 -u "${agent}" system-resume` })
    await expect(details.getByText('No active main session.')).toBeVisible()
  } finally {
    if (botId) await api(page, 'deleteBot', { botId }).catch(() => {})
    await browser.close()
  }
})
