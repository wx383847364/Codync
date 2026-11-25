import { test, expect } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { SANDBOX_PREFIX } from '../src/shared/dev-sandbox.ts'
import { api, attach, sandbox, typeMessage } from './sandbox.ts'

type Bot = { id: string; name: string; rev: number }
type Entry = { id: string; botId: string; data: {text?: string; deliveryRows?: {deliveryId: string; status: string; detail: string; resultId?: string}[];
  files?: {id: string; name: string}[] } }

test('the original ordinary and stage assignment cards receive local results and downloadable evidence', async () => {
  // 只占用本测试的沙箱；命令产物与真实项目分离。
  test.setTimeout(60_000)
  const { browser, page } = await attach()
  const root = mkdtempSync(join(tmpdir(), 'codync-local-task-e2e-'))
  const registry = join(homedir(), SANDBOX_PREFIX + sandbox().name, 'local-tasks.json')
  const previous = existsSync(registry) ? readFileSync(registry, 'utf8') : undefined
  const python = execFileSync(process.platform === 'win32' ? 'python' : 'python3',
    ['-c', 'import sys; print(sys.executable)'], { encoding: 'utf8' }).trim()
  const command = `"${python}" -u "${join(__dirname, '../../../host/tests/fixtures/team_agent.py')}"`
  const runner = join(root, 'local_task_fixture.py')
  const regularCwd = join(root, 'regular')
  const stageCwd = join(root, 'stage')
  mkdirSync(regularCwd)
  mkdirSync(stageCwd)
  writeFileSync(runner, `
from pathlib import Path
import json, os, sys, time
release, outcome = sys.argv[1:]
Path(release + '.pid').write_text(str(os.getpid()), encoding='utf-8')
while not Path(release).exists():
    time.sleep(.05)
Path('qa-evidence.txt').write_text('evidence for ' + outcome, encoding='utf-8')
Path(os.environ['CODYNC_TASK_RESULT']).write_text(json.dumps({
    'status':outcome,'summary':'Verified by the fixture: ' + outcome,'evidence':['qa-evidence.txt']
}), encoding='utf-8')
`)
  const rooms: string[] = []
  const bots: string[] = []
  try {
    const { bot: role } = await api<{ bot: Bot }>(page, 'createBot', {
      name: 'Local task QA', backend: 'opencode', cwd: regularCwd, permission: 'auto', connectors: [], skills: [], notify: false,
    })
    bots.push(role.id)
    const { bot: ordinary } = await api<{ bot: Bot }>(page, 'createBot', {
      name: 'Local task ordinary', kind: 'group', members: [role.id],
    })
    rooms.push(ordinary.id)
    const { bot: stage, createdMembers } = await api<{ bot: Bot; createdMembers: string[] }>(page, 'createStageGroup', {
      botId: ordinary.id, requestId: randomUUID(), name: 'Local task stage', leaderId: role.id,
      roles: [{ botId: role.id, sourceRev: role.rev, description: 'Run the fixture' }],
    })
    rooms.push(stage.id)
    // 阶段组使用正常创建入口，随后只为本测试把两位角色换成确定性 ACP 夹具。
    await api(page, 'updateBot', { id: role.id, backend: 'custom', command })
    await api(page, 'updateBot', { id: createdMembers[0], backend: 'custom', command, cwd: stageCwd })
    const cases = [
      {room: ordinary, cwd: regularCwd, outcome: 'passed', state: 'completed', label: 'Passed'},
      {room: stage, cwd: stageCwd, outcome: 'blocked', state: 'failed', label: 'Blocked'},
    ]
    const tasks = Object.fromEntries(['passed', 'blocked', 'stop'].map(outcome => [outcome, {
      title: `Local check ${outcome}`, program: python, args: [runner, join(root, `${outcome}.release`), outcome], timeoutSeconds: 30,
    }]))
    writeFileSync(registry, JSON.stringify({ ...JSON.parse(previous ?? '{}'), ...tasks }))
    for (const {room, cwd, outcome, state, label} of cases) {
      writeFileSync(join(cwd, 'group-script.json'), JSON.stringify({localTask: outcome, reply: 'Model handed off the run'}))
      await page.locator(`[data-roster-row$="/${room.id}"]`).click()
      await typeMessage(page, 'Run the registered local check')
      const notice = page.locator('.delivery-notice').last()
      await expect(notice).toBeVisible()
      const original = await notice.getAttribute('id')
      await notice.getByRole('button').first().click()
      await expect(notice).toContainText(`Local check ${outcome} · Running`)
      writeFileSync(join(root, `${outcome}.release`), '')
      await expect(notice).toContainText(`Local check ${outcome} · ${label}`)
      expect(await notice.getAttribute('id')).toBe(original)
      await notice.getByRole('button', { name: 'View result' }).click()
      await expect(page.getByText(`Verified by the fixture: ${outcome}`, { exact: false })).toBeVisible()
      await expect(page.getByText('qa-evidence.txt', { exact: true })).toBeVisible()
      const { entries } = await api<{ entries: Entry[] }>(page, 'history', {botId: room.id})
      const row = entries.find(entry => entry.id === original)?.data.deliveryRows?.[0]
      expect(row?.status).toBe(state)
      const {entry: result, evidence: linked} = await api<{entry: Entry; evidence: Entry}>(page, 'groupDeliveryResult', {roomId: room.id, resultId: row?.resultId})
      expect(result.data.text).toContain(`Verified by the fixture: ${outcome}`)
      const evidence = entries.find(entry => entry.data.files?.length)
      expect(linked.id).toBe(evidence?.id)
      const file = evidence?.data.files?.[0]
      const fetched = await api<{data: string}>(page, 'readFile', {entryId: evidence?.id, fileId: file?.id, offset: 0})
      expect(Buffer.from(fetched.data, 'base64').toString()).toBe(`evidence for ${outcome}`)
      if (process.env.CODYNC_TASK_EVIDENCE_DIR) {
        await page.screenshot({path: join(process.env.CODYNC_TASK_EVIDENCE_DIR, `group-local-task-${outcome}.png`)})
      }

      // 成员详情总是重新读取结果，覆盖历史未载入时的同一下载入口。
      if (outcome === 'blocked') {
        await page.locator('.title-pill').click()
        await page.locator('.group-context-reading').click()
        await page.getByRole('dialog').getByRole('button', {name:'View result'}).click()
        const preview = page.getByRole('dialog', {name:'View result'})
        await expect(preview.getByText('Verified by the fixture: blocked', {exact:false})).toBeVisible()
        await expect(preview.getByText('qa-evidence.txt', {exact:true})).toBeVisible()
        await expect(preview.getByRole('button', {name:'Download'})).toBeEnabled()
        if (process.env.CODYNC_TASK_EVIDENCE_DIR) {
          await page.screenshot({path: join(process.env.CODYNC_TASK_EVIDENCE_DIR, 'group-local-task-result.png')})
        }
        await page.keyboard.press('Escape')
        await page.keyboard.press('Escape')
      }
    }
    // 群 Stop 后进程必须退出，释放旧入口不会迟到发布结果。
    writeFileSync(join(stageCwd, 'group-script.json'), JSON.stringify({localTask: 'stop', reply: 'Model handed off the run'}))
    await typeMessage(page, 'Start a local check to stop')
    await expect.poll(() => existsSync(join(root, 'stop.release.pid'))).toBe(true)
    const pid = Number(readFileSync(join(root, 'stop.release.pid'), 'utf8'))
    await api(page, 'stop', {botId: stage.id})
    await expect.poll(() => {
      try { process.kill(pid, 0); return true } catch { return false }
    }).toBe(false)
    writeFileSync(join(root, 'stop.release'), '')
    const stopped = page.locator('.delivery-notice').last()
    await stopped.getByRole('button').first().click()
    await expect(stopped).toContainText('已停止')
    const {entries} = await api<{entries: Entry[]}>(page, 'history', {botId: stage.id})
    const stoppedId = await stopped.getAttribute('id')
    expect(entries.find(entry => entry.id === stoppedId)?.data.deliveryRows?.[0]?.status).toBe('cancelled')
    expect(entries.filter(entry => entry.data.files?.length)).toHaveLength(1)
  } finally {
    for (const botId of rooms.reverse()) await api(page, 'purgeGroup', {botId}).catch(() => {})
    for (const botId of bots) await api(page, 'deleteBot', {botId}).catch(() => {})
    if (previous == null) rmSync(registry, {force: true})
    else writeFileSync(registry, previous)
    rmSync(root, {recursive: true, force: true})
    await browser.close()
  }
})
