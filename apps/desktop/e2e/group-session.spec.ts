import { test, expect, type Locator } from '@playwright/test'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { DatabaseSync } from 'node:sqlite'
import { join } from 'node:path'
import { api, attach, sandbox, typeMessage } from './sandbox.ts'

type Group = { id: string; name: string; members: string[] }
type Prompt = { sessionId: string; prompt: { text: string }[] }

/** 在同一帧读取两个矩形，弹窗缩放期间也能比较实际对齐。 */
async function expectInlineOptions(button: Locator) {
  await expect.poll(() => button.evaluate((trigger) => {
    const list = document.getElementById(trigger.getAttribute('aria-controls') ?? '')
    if (!list) return false
    const anchor = trigger.getBoundingClientRect()
    const options = list.getBoundingClientRect()
    return Math.abs(options.width - anchor.width) < 0.5 &&
      Math.abs(options.left - anchor.left) < 0.5 && options.top >= anchor.bottom
  })).toBe(true)
}

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
      await page.getByRole('dialog', { name: 'Create new group session' }).getByRole('button', { name: 'Create', exact: true }).click()
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

/** 普通建群通过搜索和完整名单加入，新会话可重新确认名单与负责人。 */
test('the group picker searches, adds, and clears the lead in a fresh session', async () => {
  const { browser, page } = await attach()
  const name = `Picker crew ${Date.now()}`
  const ids: string[] = []
  try {
    const bots: Group[] = []
    for (const role of ['Lead', 'Reviewer', 'Backup']) {
      const { bot } = await api<{ bot: Group }>(page, 'createBot', {
        name: `${name} ${role}`, backend: 'opencode', skills: [], connectors: [],
      })
      bots.push(bot)
      ids.push(bot.id)
    }
    const [lead, reviewer, backup] = bots as [Group, Group, Group]
    await page.getByRole('button', { name: 'New', exact: true }).click()
    await page.getByRole('menuitem', { name: 'New group chat' }).click()
    const form = page.getByRole('dialog', { name: 'New group chat' })
    await form.getByRole('textbox', { name: 'Name' }).fill(name)
    await form.getByRole('button', { name: 'Show bot list' }).click()
    await form.getByRole('option', { name: reviewer.name, exact: true }).click()
    const search = form.getByRole('combobox', { name: 'Search or add bots' })
    await search.fill(lead.name)
    await expect(form.getByRole('option')).toHaveCount(1)
    await form.getByRole('option', { name: lead.name, exact: true }).click()
    await expect(form.getByRole('option', { name: lead.name, exact: true })).toHaveAttribute('aria-selected', 'true')
    // 从仍打开的搜索列表直接点击负责人，验证关闭列表不会吞掉外部点击。
    const leadButton = form.getByRole('button', { name: /Group lead/ })
    await leadButton.click()
    const leadOptions = form.getByRole('listbox', { name: /Group lead/ })
    await expect(leadOptions.getByRole('option', { name: backup.name })).toHaveCount(0)
    await expect(leadOptions.getByRole('option', { name: 'No lead' })).toHaveAttribute('aria-selected', 'true')
    await expectInlineOptions(leadButton)
    await leadOptions.getByRole('option', { name: lead.name, exact: true }).click()
    await expect(leadOptions).toBeHidden()
    await expect(leadButton).toBeFocused()
    await expect(leadButton).toHaveAccessibleDescription(lead.name)
    await form.getByRole('button', { name: 'Create', exact: true }).click()
    const selected = page.locator('.roster-row[aria-current=true]')
    await expect(selected.getByText(name, { exact: true })).toBeVisible()
    ids.unshift((await selected.getAttribute('data-roster-row'))!.split('/').at(-1)!)
    await expect(page.locator('.composer textarea')).toHaveAttribute('placeholder', `Ask ${lead.name} · @ to choose another recipient`)

    await selected.click({ button: 'right' })
    await page.getByRole('menuitem', { name: 'Create new group session' }).click()
    const fresh = page.getByRole('dialog', { name: 'Create new group session' })
    await expect(fresh.getByRole('button', { name: /Group lead/ })).toContainText(lead.name)
    await fresh.getByRole('button', { name: `Remove ${lead.name}`, exact: true }).click()
    await expect(fresh.getByRole('button', { name: /Group lead/ })).toContainText('No lead')
    await fresh.getByRole('combobox', { name: 'Search or add bots' }).fill(backup.name)
    await fresh.getByRole('option', { name: backup.name, exact: true }).click()
    await fresh.getByRole('button', { name: 'Create', exact: true }).click()
    await expect(selected.getByText(`${name} 1`, { exact: true })).toBeVisible()
    ids.unshift((await selected.getAttribute('data-roster-row'))!.split('/').at(-1)!)
    await expect(page.locator('.composer textarea')).toHaveAttribute('placeholder', 'Ask everyone · @ to ask one bot')
    await expect(page.locator('.intro-card')).toContainText(backup.name)
    await expect(page.locator('.intro-card')).not.toContainText(lead.name)
  } finally {
    // 移除测试群和机器人，便于继续在同一沙箱检查。
    for (const id of ids) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    await browser.close()
  }
})

/** 真实群菜单打开阶段表单，新建后显示新负责人、空白历史和待发送的目标。 */
test('stage opens with fresh roles, saves a versioned handoff and closes independently', async () => {
  const { browser, page } = await attach()
  const ids: string[] = []
  const name = `Stage fixture ${Date.now()}`
  try {
    // 仅保存标准后端配置，新建过程无需安装或调用代理。
    const { bot: reviewer } = await api<{ bot: Group }>(page, 'createBot', {
      name: `${name} Reviewer`, backend: 'opencode', description: 'Review requirements', skills: [], connectors: [],
    })
    ids.push(reviewer.id)
    const { bot: lead } = await api<{ bot: Group & { rev: number } }>(page, 'createBot', {
      name: `${name} Lead`, backend: 'opencode', description: 'Coordinate work', skills: [], connectors: [],
    })
    ids.push(lead.id)
    const { bot: original } = await api<{ bot: Group }>(page, 'createBot', {
      name, kind: 'group', members: [reviewer.id, lead.id],
    })
    ids.unshift(original.id)
    await page.locator('.roster-row').getByText(name, { exact: true }).click({ button: 'right' })
    // 用户在选择前能看到两种创建方式，读屏也会读出说明。
    const sessionAction = page.getByRole('menuitem', { name: 'Create new group session', exact: true })
    const stageAction = page.getByRole('menuitem', { name: 'Create next stage group', exact: true })
    await expect(sessionAction.getByText('Original members are preselected. You can change them.')).toBeVisible()
    await expect(sessionAction).toHaveAccessibleDescription('Original members are preselected. You can change them.')
    await stageAction.click()
    await page.getByPlaceholder('Stage name').fill(`${name} next`)
    await page.getByPlaceholder('Stage goal (optional)').fill('Build the next phase')
    await page.getByPlaceholder('Acceptance criteria (optional)').fill('A verified result is visible')
    await page.getByRole('button', { name: 'Show bot list' }).click()
    await page.getByRole('option', { name: reviewer.name, exact: true }).click()
    await page.getByRole('option', { name: lead.name, exact: true }).click()
    await page.getByRole('button', { name: /Group lead/ }).click()
    await page.getByRole('listbox', { name: /Group lead/ }).getByRole('option', { name: lead.name, exact: true }).click()
    // 模拟负责人在另一窗口被标记为已读。同步版本变化，角色配置不变。
    const rosterDb = new DatabaseSync(join(homedir(), `.codync-dev-${sandbox().name}`, 'codync.db'))
    try {
      const before = rosterDb.prepare('SELECT rev, config FROM bots WHERE id = ?').get(lead.id)!
      rosterDb.prepare(
        "INSERT INTO entries(id, bot_id, rev, kind, turn, data, created_at, updated_at) VALUES (?, ?, ?, 'agent', 1, ?, ?, ?)",
      ).run(`${lead.id}-read-fixture`, lead.id, before.rev, '{"text":"Ready for review","final":true}', Date.now(), Date.now())
      await api(page, 'markRead', { botId: lead.id })
      const after = rosterDb.prepare('SELECT rev, config FROM bots WHERE id = ?').get(lead.id)!
      expect(Number(after.rev)).toBeGreaterThan(Number(before.rev))
      expect(after.config).toBe(before.config)
    } finally { rosterDb.close() }
    await page.getByRole('button', { name: 'Role settings', exact: true }).click()
    const roles = page.getByRole('dialog', { name: 'Role settings' })
    await expect(roles.getByPlaceholder('Role instructions')).toHaveValue('Review requirements')
    const roleButton = roles.getByRole('button', { name: 'Role settings', exact: true })
    await roleButton.click()
    const roleOptions = roles.getByRole('listbox', { name: 'Role settings' })
    await expect(roleOptions.getByRole('option', { name: reviewer.name, exact: true })).toHaveAttribute('aria-selected', 'true')
    await expectInlineOptions(roleButton)
    await page.keyboard.press('Escape')
    await expect(roleOptions).toBeHidden()
    await expect(roles).toBeVisible()
    await roleButton.click()
    await roleOptions.getByRole('option', { name: lead.name, exact: true }).click()
    await expect(roles.getByPlaceholder('Role instructions')).toHaveValue('Coordinate work')
    await expect(roleButton).toHaveAccessibleDescription(lead.name)
    await roles.getByRole('button', { name: 'Done', exact: true }).click()
    await page.getByRole('button', { name: 'Create', exact: true }).click()
    const selected = page.locator('.roster-row[aria-current=true]')
    await expect(selected.getByText(`${name} next`, { exact: true })).toBeVisible()
    const id = (await selected.getAttribute('data-roster-row'))!.split('/').at(-1)!
    ids.unshift(id)
    const db = new DatabaseSync(join(homedir(), `.codync-dev-${sandbox().name}`, 'codync.db'), { readOnly: true })
    try {
      // 确认负责人绑定到新实例，来源成员名单与配置保持原样。
      const get = db.prepare('SELECT config FROM bots WHERE id = ?')
      const group = JSON.parse(get.get(id)!.config as string)
      ids.push(...group.members)
      expect(group.members).toHaveLength(2)
      expect(group.stage.ownedMembers).toEqual(group.members)
      for (const member of group.members) {
        await expect(page.locator(`[data-roster-row$="/${member}"]`)).toHaveCount(0)
      }
      await expect(page.locator(`[data-roster-row$="/${lead.id}"]`)).toBeVisible()
      const heading = selected.locator('..')
      await heading.getByRole('button', { name: 'Expand stage members' }).click()
      await expect(heading.locator('..').locator(`[data-roster-row$="/${group.members[1]}"]`)).toBeVisible()
      await heading.getByRole('button', { name: 'Collapse stage members' }).click()
      // 旧群可以展开复用的项目机器人，阶段群与旧群可在同一项目内拖动互换位置。
      const old = page.locator(`[data-roster-row$="/${original.id}"]`)
      const oldHeading = old.locator('..')
      await oldHeading.getByRole('button', { name: 'Expand stage members' }).click()
      await expect(oldHeading.locator('..').locator('.stage-member').getByText(lead.name, { exact: true })).toBeVisible()
      await expect(page.locator(`[data-roster-row$="/${lead.id}"]`)).toHaveCount(1)
      await oldHeading.getByRole('button', { name: 'Collapse stage members' }).click()
      const rows = page.locator('.roster-row')
      const stageKey = (await selected.getAttribute('data-roster-row'))!
      const oldKey = (await old.getAttribute('data-roster-row'))!
      const order = async () => rows.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-roster-row')))
      const before = await order()
      const originallyFirst = before.indexOf(stageKey) < before.indexOf(oldKey)
      await selected.dragTo(old)
      await expect.poll(async () => {
        const after = await order()
        return after.indexOf(stageKey) < after.indexOf(oldKey)
      }).toBe(!originallyFirst)
      const position = db.prepare("SELECT json_extract(config, '$.position') AS value FROM bots WHERE id = ?")
      await expect.poll(() => (Number(position.get(id)?.value) - Number(position.get(original.id)?.value))
        * (originallyFirst ? 1 : -1)).toBeGreaterThan(0)
      expect(group.members.every((member: string) => member !== reviewer.id && member !== lead.id)).toBe(true)
      expect(group.stage.leaderId).toBe(group.members[1])
      expect(JSON.parse(get.get(original.id)!.config as string).members).toEqual([reviewer.id, lead.id])
      expect(JSON.parse(get.get(group.members[1])!.config as string).sharedMemory).toBe(false)
    } finally { db.close() }
    await expect(page.getByText(`Default recipient · ${lead.name}`)).toBeVisible()
    await expect(page.locator('.composer textarea')).toHaveValue(/Build the next phase[\s\S]*A verified result is visible/)
    expect((await api<{ entries: unknown[] }>(page, 'history', { botId: id })).entries).toEqual([])

    // 真正修改角色指令后，Host 仍拒绝用旧配置版本创建另一群。
    const { bot: changed } = await api<{ bot: Group & { rev: number } }>(page, 'updateBot', {
      id: lead.id, description: 'Updated coordination instructions',
    })
    expect(changed.rev).toBeGreaterThan(lead.rev)
    const sandboxToken = readFileSync(join(homedir(), `.codync-dev-${sandbox().name}`, 'token'), 'utf8').trim()
    const conflict = await fetch(`http://127.0.0.1:${sandbox().port}/api/createStageGroup`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${sandboxToken}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({
        botId: original.id, requestId: crypto.randomUUID(),
        roles: [{ botId: lead.id, sourceRev: lead.rev, description: 'Coordinate work' }],
      }),
    })
    expect(conflict.status).toBe(400)
    expect((await conflict.json()).error).toContain('changed while you were reviewing the role')

    // 通过真实弹层保存、验收和结束，再选择同一版本创建下一阶段。
    const stage = page.locator(`[data-roster-row$="/${id}"]`)
    await stage.click({ button: 'right' })
    // 先从尚未验收的阶段创建后继，只选择一个机器人。
    await stageAction.click()
    const form = page.getByRole('dialog', { name: 'Create next stage group' })
    const create = form.getByRole('button', { name: 'Create', exact: true })
    await expect(create).toBeDisabled()
    await form.getByRole('combobox', { name: 'Search or add bots' }).fill('Lead')
    await form.getByRole('option', { name: lead.name, exact: true }).click()
    await expect(create).toBeEnabled()
    await create.click()
    const generatedName = `${name} next · ${lead.name}`
    await expect(selected.getByText(generatedName, { exact: true })).toBeVisible()
    const generated = (await selected.getAttribute('data-roster-row'))!.split('/').at(-1)!
    ids.unshift(generated)
    const generatedDetails = await api<{ bot: Group & { stage: { leaderId: string } }; initialDraft: string }>(
      page, 'stageDetails', { botId: generated },
    )
    ids.push(...generatedDetails.bot.members)
    expect(generatedDetails.bot.stage.leaderId).toBe('')
    expect(generatedDetails.initialDraft).toBe('')
    await expect(page.locator('.composer textarea')).toHaveAttribute('placeholder', 'Ask everyone · @ to ask one bot')
    // 切回来源阶段，再验收交接并验证该阶段关闭后的聊天状态。
    await stage.click()
    await stage.click({ button: 'right' })
    await page.getByRole('menuitem', { name: 'Stage details' }).click()
    const handoff = '# Stage result\n\nVerified marker and fixed constraints.'
    await page.getByPlaceholder('Handoff Markdown').fill(handoff)
    await page.getByPlaceholder('Durable evidence references').fill('Fixture repository commit abc123')
    await page.getByRole('button', { name: 'Save handoff' }).click()
    await expect(page.getByRole('button', { name: 'Accept stage' })).toBeEnabled()
    // 模拟另一端保存新版，刷新运行状态后旧编辑仍只能用原基准版本提交。
    const saved = await api<{ bot: { stage: { handoff: { file: { id: string } } } } }>(page, 'stageDetails', { botId: id })
    await page.getByPlaceholder('Handoff Markdown').fill('Stale editor text')
    await api(page, 'changeStage', { botId: id, action: 'saveHandoff', markdown: handoff,
      evidence: 'Fixture repository commit def456', previousVersion: saved.bot.stage.handoff.file.id })
    await page.getByRole('button', { name: 'Refresh' }).click()
    await page.getByRole('button', { name: 'Save handoff' }).click()
    await expect(page.getByText(/The handoff changed on another device/)).toBeVisible()
    await page.keyboard.press('Escape')
    await stage.click({ button: 'right' })
    await page.getByRole('menuitem', { name: 'Stage details' }).click()
    await page.getByRole('button', { name: 'Accept stage' }).click()
    await expect(page.getByText('Stage accepted', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Close stage' }).click()
    await expect(page.getByRole('button', { name: 'Reopen stage' })).toBeVisible()
    await page.getByRole('button', { name: 'Stage usage' }).click()
    await expect(page.getByText('Host attempts · 0', { exact: true })).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page.locator('.composer').getByText('Stage closed', { exact: true })).toBeVisible()
    // 归档移动整群，历史仍可查看；恢复后仍保持归档前的关闭状态。
    await stage.click({ button: 'right' })
    await page.getByRole('menuitem', { name: 'Archive stage', exact: true }).click()
    await expect(page.locator('.composer').getByText('Stage archived', { exact: true })).toBeVisible()
    await expect(stage).toHaveCount(0)
    await page.getByRole('button', { name: 'Archived stages', exact: true }).click()
    await expect(stage).toBeVisible()
    await stage.click({ button: 'right' })
    await page.getByRole('menuitem', { name: 'Restore stage', exact: true }).click()
    await expect(page.locator('.composer').getByText('Stage closed', { exact: true })).toBeVisible()
    await expect(stage).toBeVisible()

    await stage.click({ button: 'right' })
    await stageAction.click()
    await page.getByPlaceholder('Stage name').fill(`${name} successor`)
    // 两个阶段字段同时留空，交接仍可由现有开关明确带入。
    await page.getByRole('switch', { name: 'Use accepted handoff' }).click()
    await page.getByRole('combobox', { name: 'Search or add bots' }).fill('Lead')
    await page.getByRole('option', { name: lead.name, exact: true }).click()
    await page.getByRole('button', { name: /Group lead/ }).click()
    await page.getByRole('listbox', { name: /Group lead/ }).getByRole('option', { name: lead.name, exact: true }).click()
    await page.getByRole('button', { name: 'Create', exact: true }).click()
    await expect(selected.getByText(`${name} successor`, { exact: true })).toBeVisible()
    const successor = (await selected.getAttribute('data-roster-row'))!.split('/').at(-1)!
    ids.unshift(successor)
    const details = await api<{ bot: Group; incomingMarkdown: string }>(page, 'stageDetails', { botId: successor })
    ids.push(...details.bot.members)
    await expect(page.locator('.composer textarea')).toHaveValue('')
    expect(details.incomingMarkdown).toBe(`${handoff}\n\n## Evidence references\n\nFixture repository commit def456\n`)
    expect((await api<{ entries: unknown[] }>(page, 'history', { botId: successor })).entries).toEqual([])
  } finally {
    // 测试结束后回收隔离工作区中的测试群和机器人。
    for (const id of ids) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    await browser.close()
  }
})
