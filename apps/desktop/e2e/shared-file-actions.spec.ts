import { test, expect } from '@playwright/test'
import { mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { api, attach, typeMessage } from './sandbox.ts'

test('a generated file card keeps its file actions after the original is deleted', async ({}, testInfo) => {
  const { browser, page } = await attach()
  const folder = mkdtempSync(join(tmpdir(), 'codync-shared-card-test-'))
  const original = join(folder, 'QA report.md')
  const snapshot = '# Retest complete\n\n共通过 4 组检查。\n'
  let botId: string | undefined
  let groupId: string | undefined
  let copyPath: string | undefined
  try {
    await page.context().grantPermissions(['clipboard-read'], { origin: new URL(page.url()).origin })
    writeFileSync(original, snapshot)
    writeFileSync(join(folder, 'selected-file.txt'), original)
    const fixture = resolve(__dirname, '../../../host/tests/fixtures/file_agent.py')
    const name = `Shared card ${Date.now()}`
    botId = (await api<{ bot: { id: string } }>(page, 'createBot', {
      name, backend: 'custom', command: `python3 -X utf8 -u '${fixture}'`, cwd: folder, connectors: [], skills: [], notify: false,
    })).bot.id
    groupId = (await api<{ bot: { id: string } }>(page, 'createBot', {
      name: `${name} group`, kind: 'group', members: [botId], notify: false,
    })).bot.id
    await page.locator('.roster-row', { hasText: `${name} group` }).click()
    await typeMessage(page, 'share-selected-file')
    const card = page.locator('.file-card').filter({ hasText: 'QA report.md' })
    await expect(card).toBeVisible()
    unlinkSync(original)
    await page.reload()
    await page.locator('.roster-row', { hasText: `${name} group` }).click()

    await card.click({ button: 'right' })
    const menu = page.getByRole('menu')
    await expect(menu.getByRole('menuitem', { name: 'Open', exact: true })).toBeVisible()
    await expect(menu.getByRole('menuitem', { name: 'Save as…', exact: true })).toBeVisible()
    await expect(menu.getByRole('menuitem', { name: process.platform === 'darwin' ? 'Show in Finder' : 'Show in file manager' })).toBeVisible()
    await expect(menu.getByRole('menuitem', { name: 'Show what it did' })).toBeVisible()
    if (process.platform === 'darwin') await expect(menu.getByRole('menuitem', { name: 'Open with…' })).toBeVisible()
    await page.screenshot({ path: testInfo.outputPath('shared-file-actions.png'), animations: 'disabled' })

    await menu.getByRole('menuitem', { name: 'Copy file contents' }).click()
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(snapshot)
    await expect(menu).toHaveCount(0)

    const nameButton = card.getByRole('button', { name: 'QA report.md', exact: true })
    await nameButton.focus()
    await page.keyboard.press('Enter')
    await menu.getByRole('menuitem', { name: 'Copy path' }).click()
    await expect.poll(async () => {
      const path = await page.evaluate(() => navigator.clipboard.readText())
      if (path.endsWith('QA report.md') && dirname(path).includes('codync-shared-file-')) copyPath = path
      return copyPath ? readFileSync(copyPath, 'utf8') : null
    }).toBe(snapshot)
  } finally {
    if (copyPath && dirname(copyPath).includes('codync-shared-file-')) rmSync(dirname(copyPath), { recursive: true, force: true })
    if (groupId) await api(page, 'deleteBot', { botId: groupId }).catch(() => {})
    if (botId) await api(page, 'deleteBot', { botId }).catch(() => {})
    await page.context().clearPermissions()
    await browser.close()
    rmSync(folder, { recursive: true, force: true })
  }
})
