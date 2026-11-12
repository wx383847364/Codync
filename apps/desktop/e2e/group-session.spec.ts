import { test, expect } from '@playwright/test'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { DatabaseSync } from 'node:sqlite'
import { join } from 'node:path'
import { api, attach, sandbox, typeMessage } from './sandbox.ts'

type Group = { id: string; name: string; members: string[] }
type Prompt = { sessionId: string; prompt: { text: string }[] }

/** 从真实右键菜单创建新群，检查导航、编号、成员顺序和实际 ACP 会话。 */
test('right-click opens an empty numbered group with its own member sessions', async () => {
  const { browser, page } = await attach()
  const workspace = mkdtempSync(join(tmpdir(), 'codync-group-session-'))
  const name = `Session crew ${Date.now()}`
  const members: string[] = []
  const groups: string[] = []
  try {
    for (const suffix of ['B', 'A']) {
      const cwd = join(workspace, suffix)
      mkdirSync(cwd)
      const { bot } = await api<{ bot: Group }>(page, 'createBot', {
        name: `${name} ${suffix}`, backend: 'custom', cwd,
        command: `python3 -u '${join(__dirname, '../../../host/tests/fixtures/team_agent.py')}'`, connectors: [], skills: [], notify: false,
      })
      members.push(bot.id)
    }
    // 第二个编号群用真实 STDIO 工具桥把任务交给另一成员。
    for (const suffix of ['B', 'A']) {
      const steps = ['old reply', 'fresh reply', 'continuing reply', 'second group reply', 'scoped handoff reply'].map((reply) => ({
        reply: `${suffix} ${reply}`,
        ...(suffix === 'B' && reply === 'second group reply' ? { tasks: [{ botId: members[1], message: 'Verify the current group tool scope' }] } : {}),
      }))
      writeFileSync(join(workspace, suffix, 'group-script.json'), JSON.stringify({ steps }))
    }
    const { bot: original } = await api<{ bot: Group }>(page, 'createBot', { name, kind: 'group', members })
    groups.push(original.id)
    const source = page.locator('.roster-row').getByText(name, { exact: true })
    await source.click()
    await typeMessage(page, 'ORIGINAL_GROUP_MARKER')
    await expect(page.getByText('A old reply', { exact: true })).toBeVisible()

    for (const number of [1, 2, 3]) {
      await source.click({ button: 'right' })
      await page.getByRole('menuitem', { name: 'Create new group session' }).click()
      const selected = page.locator('.roster-row[aria-current=true]')
      await expect(selected.getByText(`${name} ${number}`, { exact: true })).toBeVisible()
      const id = (await selected.getAttribute('data-roster-row'))!.split('/').at(-1)!
      groups.push(id)
      // 核对 Host 已保存的成员顺序，名单头像可能只展示前几个成员。
      const db = new DatabaseSync(join(homedir(), `.codync-dev-${sandbox().name}`, 'codync.db'), { readOnly: true })
      try {
        const row = db.prepare('SELECT config FROM bots WHERE id = ?').get(id)
        expect(JSON.parse(row!.config as string).members).toEqual(members)
      } finally { db.close() }
      expect((await api<{ entries: unknown[] }>(page, 'history', { botId: id })).entries).toEqual([])
      await expect(page.getByText('ORIGINAL_GROUP_MARKER', { exact: true })).toHaveCount(0)
      if (number === 1) {
        await typeMessage(page, 'FIRST_GROUP_MARKER')
        await expect(page.getByText('A fresh reply', { exact: true })).toBeVisible()
        await typeMessage(page, 'Continue in the first new group')
        await expect(page.getByText('A continuing reply', { exact: true })).toBeVisible()
      } else if (number === 2) {
        await typeMessage(page, 'BLOCK Begin the second new group')
        const requests = join(workspace, 'B', 'prompt-requests.jsonl')
        await expect.poll(() => readFileSync(requests, 'utf8')).toContain('BLOCK Begin the second new group')
        // 当前成员正在第二群执行，第一群的迟到请求必须在派工前被拒绝。
        await expect(api(page, 'teamCall', {
          botId: members[0], groupSession: groups[1], name: 'message_bot',
          arguments: { botId: members[1], message: 'OLD_TOOL_SCOPE_MARKER' },
        })).rejects.toThrow()
        writeFileSync(join(workspace, 'B', 'release'), '')
        writeFileSync(join(workspace, 'A', 'release'), '')
        await expect(page.getByText('A second group reply', { exact: true })).toBeVisible()
        await expect(page.getByText('A scoped handoff reply', { exact: true })).toBeVisible()
      }
    }
    for (const suffix of ['B', 'A']) {
      // 对比实际发送给代理的身份和提示词，空白转录本身无法证明上下文隔离。
      const prompts: Prompt[] = readFileSync(join(workspace, suffix, 'prompt-requests.jsonl'), 'utf8').trim().split('\n').map((line) => JSON.parse(line))
      expect(prompts).toHaveLength(suffix === 'A' ? 5 : 4)
      expect(new Set(prompts.map((turn) => turn.sessionId)).size).toBe(3)
      expect(prompts[1]!.sessionId).toBe(prompts[2]!.sessionId)
      expect(prompts.slice(1).map((turn) => turn.prompt[0]!.text).join('\n')).not.toContain('ORIGINAL_GROUP_MARKER')
      expect(prompts[3]!.prompt[0]!.text).not.toContain('FIRST_GROUP_MARKER')
      expect(prompts.slice(3).map((turn) => turn.prompt[0]!.text).join('\n')).not.toContain('OLD_TOOL_SCOPE_MARKER')
      if (suffix === 'A') expect(prompts[4]!.prompt[0]!.text).toContain('Verify the current group tool scope')
    }
  } finally {
    // 无论断言结果如何，都移除本用例创建的群、成员和工作目录。
    for (const id of [...groups, ...members]) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    rmSync(workspace, { recursive: true, force: true })
    await browser.close()
  }
})
