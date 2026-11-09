import { test, expect } from '@playwright/test'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { DatabaseSync } from 'node:sqlite'
import { join } from 'node:path'
import { homedir, tmpdir } from 'node:os'
import { api, attach, sandbox, steerBot, typeMessage } from './sandbox.ts'

type History = { entries: { kind: string; data: { final?: boolean; text?: string; mentions?: { botId: string; start: number; end: number }[] } }[] }

/** 用真实群聊验证同时显示工作的成员，且忙成员排队后仍收到原消息。 */
test('idle group members answer a new message in order while a member finishes earlier work', async () => {
  const { browser, page } = await attach()
  const suffix = Date.now()
  const workspace = join(tmpdir(), `codync-group-busy-${suffix}`)
  const names = [`BusyA${suffix}`, `BusyB${suffix}`, `BusyC${suffix}`]
  const original = '@all BLOCK review this exact original 𠀀🙂'
  const firstReply = 'B received the group message'
  const secondReply = 'C received it after B'
  const lateReply = 'A received the original after finishing'
  const ids: string[] = []
  let group: string | undefined
  try {
    for (const [i, name] of names.entries()) {
      const cwd = join(workspace, `${i}`)
      mkdirSync(cwd, { recursive: true })
      ids.push((await api<{ bot: { id: string } }>(page, 'createBot', {
        name, backend: 'custom', cwd, command: `python3 -u '${join(__dirname, '../../../host/tests/fixtures/team_agent.py')}'`, connectors: [], skills: [], notify: false,
      })).bot.id)
    }
    writeFileSync(join(workspace, '0', 'group-script.json'), JSON.stringify({ steps: [{ reply: 'A finished earlier work' }, { reply: lateReply }] }))
    writeFileSync(join(workspace, '1', 'group-script.json'), JSON.stringify({ steps: [{ reply: firstReply }, { reply: '(pass)' }] }))
    writeFileSync(join(workspace, '2', 'group-script.json'), JSON.stringify({ reply: secondReply }))
    writeFileSync(join(workspace, '2', 'release'), '')
    writeFileSync(join(workspace, '0', 'block-thought.txt'), 'A is thinking about the earlier assignment')

    const name = `Busy delivery room ${suffix}`
    group = (await api<{ bot: { id: string } }>(page, 'createBot', { name, kind: 'group', members: ids })).bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    const aPrompts = join(workspace, '0', 'prompts.jsonl')
    const bPrompts = join(workspace, '1', 'prompts.jsonl')
    const cPrompts = join(workspace, '2', 'prompts.jsonl')
    await typeMessage(page, `@${names[0]} @${names[1]} BLOCK finish the earlier assignment`)
    // 夹具写下 ACP 入参后才算开始，避免把群消息刚入库误判为成员已收到。
    await expect.poll(() => existsSync(aPrompts) && readFileSync(aPrompts, 'utf8').includes('finish the earlier assignment')).toBe(true)
    expect(existsSync(bPrompts)).toBe(false)
    await typeMessage(page, original)
    // B 已收到本轮正文但尚未回复，C 应仍在等待。
    await expect.poll(() => existsSync(bPrompts) && readFileSync(bPrompts, 'utf8').includes('review this exact original')).toBe(true)
    expect(existsSync(cPrompts)).toBe(false)
    // A 与 B 同时在工作，C 此时还在等本轮顺序。
    const indicators = page.locator('.group-working-member')
    const aActivity = indicators.filter({ hasText: names[0] })
    const bActivity = indicators.filter({ hasText: names[1] })
    await expect(aActivity).toContainText(`${names[0]}: Thinking…`)
    await expect(bActivity).toBeVisible()
    await expect(indicators.filter({ hasText: names[2] })).toHaveCount(0)
    await expect(aActivity.getByText(/^\d+:\d{2}$/)).toBeVisible()
    await expect(bActivity.getByText(/^\d+:\d{2}$/)).toBeVisible()
    const aBounds = await aActivity.boundingBox()
    const bBounds = await bActivity.boundingBox()
    expect(bBounds!.x).toBeGreaterThan(aBounds!.x)
    expect(bBounds!.y).toBe(aBounds!.y)
    // 只收窄活动区验证换行，避免改变共用 Electron 沙箱的窗口设置。
    const activityRow = page.locator('.group-working')
    await activityRow.evaluate((row) => {
      // 模拟窄会话可分配的空间。
      row.style.maxWidth = '220px'
    })
    try {
      expect((await bActivity.boundingBox())!.y).toBeGreaterThan((await aActivity.boundingBox())!.y)
    } finally {
      await activityRow.evaluate((row) => {
        // 复原活动区，继续验证派工行为。
        row.style.removeProperty('max-width')
      })
    }
    writeFileSync(join(workspace, '1', 'release'), '')
    await expect(page.getByText(firstReply, { exact: true })).toBeVisible()
    await expect(page.getByText(secondReply, { exact: true })).toBeVisible()
    // C 的实际 ACP 入参包含 B 刚提交的回复，A 的旧任务此时仍被夹具阻塞。
    expect(readFileSync(cPrompts, 'utf8')).toContain(firstReply)
    await expect(page.getByText('A finished earlier work', { exact: true })).toHaveCount(0)
    // B 结束后只移除 B 的标签，A 仍在处理原来的任务。
    await expect(bActivity).toHaveCount(0)
    await expect(aActivity).toBeVisible()

    writeFileSync(join(workspace, '0', 'release'), '')
    await expect(page.getByText(lateReply, { exact: true })).toBeVisible()
    const prompts = readFileSync(aPrompts, 'utf8').trim().split('\n')
    expect(prompts).toHaveLength(2)
    expect(JSON.parse(prompts[1])).toContain(`Request (complete original):\nUser: ${original}`)
  } finally {
    // 某项断言失败时也清理本用例的群和成员，让后续用例继续使用同一沙箱。
    if (group) await api(page, 'deleteBot', { botId: group }).catch(() => {})
    for (const id of ids) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    rmSync(workspace, { recursive: true, force: true })
    await browser.close()
  }
})

/** 超过旧轮次、发言与深度上限后交付给用户，状态仍折叠且不重复来源正文。 */
test('relay compacts bounded background past fixed limits while preserving originals and open work', async () => {
  const { browser, page } = await attach()
  const suffix = Date.now()
  const workspace = join(tmpdir(), `codync-compact-relay-${suffix}`)
  const marker = `SOURCE_REPLY_${suffix}`
  const original = `@StatusA${suffix} ${'原始任务𠀀😀保持附件与验收要求。'.repeat(600)} ROOT_REQUIREMENT_TAIL_${suffix}`
  const names = [`StatusA${suffix}`, `StatusB${suffix}`]
  const ids: string[] = []
  let group: string | undefined
  try {
    for (let i = 0; i < 2; i++) {
      const cwd = join(workspace, `${i}`)
      mkdirSync(cwd, { recursive: true })
      // 夹具只产生确定的回复；实际路由、轮次、取消和卡片仍由 host 完成。
      ids.push((await api<{ bot: { id: string } }>(page, 'createBot', { name: names[i], backend: 'custom', cwd, command: `python3 -u '${join(__dirname, '../../../host/tests/fixtures/team_agent.py')}'`, connectors: [], skills: [], notify: false })).bot.id)
    }
    for (let i = 0; i < 2; i++) {
      // 二十五次真实 ACP turn 混合文字与 MCP 工具派工，最终交付用户。
      const steps = Array.from({ length: i === 0 ? 13 : 12 }, (_, step) => ({
        reply: i === 0 && step === 12
          ? `USER_HANDOFF_${suffix} 已交付实现与审核结果，请用户确认。`
          : `@${names[1 - i]} ${marker} **结论**\n${'仅在来源消息显示此长段说明。'.repeat(30)}`,
        // 真实 usage 触发领取前原生维护；开放计划不能被后续接力背景挤掉。
        ...(i === 0 && step === 0 ? { usage: { used: 70_000, size: 200_000 }, plan: [{ content: `OPEN_TODO_${suffix}`, status: 'in_progress', priority: 'high' }] } : {}),
        tasks: step % 2 === 1 ? [{ botId: ids[1 - i], message: `继续第 ${step} 项修改和复核`, idempotencyKey: `${suffix}-${i}-${step}` }] : [],
      }))
      writeFileSync(join(workspace, `${i}`, 'group-script.json'), JSON.stringify({ steps }))
    }
    writeFileSync(join(workspace, '0', 'group-compact.json'), JSON.stringify({ usage: { used: 20_000, size: 200_000 } }))
    const name = `Compact delivery room ${suffix}`
    group = (await api<{ bot: { id: string } }>(page, 'createBot', { name, kind: 'group', members: ids })).bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    await api(page, 'send', { botId: group, text: original })
    await expect.poll(async () => (await api<History>(page, 'history', { botId: group })).entries.filter((e) => e.kind === 'agent' && e.data.final === true).length, { timeout: 25_000 }).toBe(25)
    const db = new DatabaseSync(join(homedir(), `.codync-dev-${sandbox().name}`, 'codync.db'), { readOnly: true })
    try {
      const rows = db.prepare('SELECT round,chain_depth,status FROM deliveries WHERE room_id=? ORDER BY room_seq').all(group)
      expect(rows).toHaveLength(25)
      expect(rows.every((row) => row.status === 'completed')).toBe(true)
      expect(rows.at(-1)).toMatchObject({ round: 13, chain_depth: 25 })
    } finally { db.close() }
    const card = page.locator('.delivery-notice').last()
    const toggle = card.getByRole('button', { name: '群内派工已完成', exact: true })
    await expect(toggle).toBeVisible()
    await expect(toggle).toHaveAttribute('aria-expanded', 'false')
    await expect(card.locator('.delivery-details')).toHaveCount(0)
    expect(await card.innerText()).not.toMatch(/Attempt|cancelled|SOURCE_REPLY_/)
    const evidence = join(tmpdir(), 'codync-compact-delivery-e2e')
    mkdirSync(evidence, { recursive: true })
    await page.screenshot({ path: join(evidence, 'delivery-collapsed.png') })
    await toggle.click()
    await expect(toggle).toHaveAttribute('aria-expanded', 'true')
    await expect(card.locator('.delivery-details')).toContainText('第 1 次投递')
    await expect(card.locator('.delivery-details')).toContainText(`${names[0]}：已完成`)
    await expect(page.getByText(`USER_HANDOFF_${suffix} 已交付实现与审核结果，请用户确认。`, { exact: true })).toBeVisible()
    await expect(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true })).toHaveCount(0)
    expect(await card.innerText()).not.toMatch(/Attempt|cancelled|SOURCE_REPLY_/)
    await page.screenshot({ path: join(evidence, 'delivery-expanded.png') })
    await toggle.focus()
    await page.keyboard.press('Space')
    await expect(toggle).toHaveAttribute('aria-expanded', 'false')
    // 重载后仍默认折叠，旧历史中的 deliveryBody 也不能重新铺满界面。
    await page.reload()
    await page.locator('.roster-row', { hasText: name }).click()
    await expect(toggle).toHaveAttribute('aria-expanded', 'false')
    await expect(page.locator('.delivery-details')).toHaveCount(0)
    const prompts = readFileSync(join(workspace, '0', 'prompts.jsonl'), 'utf8')
    expect(prompts).toContain('There is no fixed group round, reply-count, or handoff-depth limit')
    expect(prompts).toContain('genuinely need the user')
    // 检查真实ACP入参，而非仅看最终气泡；补充背景单独受预算，完整来源不裁切。
    const requests = readFileSync(join(workspace, '0', 'prompt-requests.jsonl'), 'utf8').trim().split('\n').map((line) => JSON.parse(line) as { sessionId: string; prompt: { text: string }[] })
    const work = requests.filter((request) => !request.prompt[0].text.startsWith('/compact'))
    expect(work).toHaveLength(13)
    expect(new Set(requests.map((request) => request.sessionId)).size).toBe(1)
    for (const request of work) {
      expect(request.prompt[0].text).toContain(original)
      const excerpt = request.prompt[0].text.split('Saved room progress excerpts')[1]?.split('Context estimates only:')[0] ?? ''
      expect(Buffer.byteLength(excerpt, 'utf8')).toBeLessThanOrEqual(8_192)
      const background = /supplementary=(\d+)\/2048/.exec(request.prompt[0].text)
      expect(Number(background?.[1])).toBeLessThanOrEqual(2_048)
    }
    expect(work.at(-1)?.prompt[0].text).toContain(`OPEN_TODO_${suffix}`)
    expect(readFileSync(join(workspace, '0', 'compactions.jsonl'), 'utf8').trim().split('\n')).toHaveLength(1)
    expect(JSON.stringify((await api<History>(page, 'history', { botId: group })).entries)).not.toContain('COMPACT_PRIVATE')
    // 交付后由用户的新消息开启下一项，不自动重放前一批已完成记录。
    await api(page, 'send', { botId: group, text: `@${names[0]} 用户确认，请继续下一项` })
    await expect.poll(async () => (await api<History>(page, 'history', { botId: group })).entries.filter((e) => e.kind === 'agent' && e.data.final === true).length).toBe(26)
    await expect(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true })).toHaveCount(0)
  } finally {
    // 仅清理此用例新建的沙盒群、成员和工作目录。
    if (group) await api(page, 'deleteBot', { botId: group }).catch(() => {})
    for (const id of ids) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    rmSync(workspace, { recursive: true, force: true })
    await browser.close()
  }
})

/** 未回复投递的重试入口位于详情中，点击后仍由 host 创建新投递和新 attempt。 */
test('folded delivery details preserve explicit retry and a new collapsed attempt', async () => {
  const { browser, page } = await attach()
  const suffix = Date.now()
  const cwd = join(tmpdir(), `codync-compact-retry-${suffix}`)
  mkdirSync(cwd, { recursive: true })
  const script = join(cwd, 'group-script.json')
  writeFileSync(script, JSON.stringify({ reply: '(pass)' }))
  let member: string | undefined
  let group: string | undefined
  try {
    member = (await api<{ bot: { id: string } }>(page, 'createBot', { name: `Retry member ${suffix}`, backend: 'custom', cwd, command: `python3 -u '${join(__dirname, '../../../host/tests/fixtures/team_agent.py')}'`, connectors: [], skills: [], notify: false })).bot.id
    const name = `Compact retry room ${suffix}`
    group = (await api<{ bot: { id: string } }>(page, 'createBot', { name, kind: 'group', members: [member] })).bot.id
    await page.locator('.roster-row', { hasText: name }).click()
    await api(page, 'send', { botId: group, text: 'retry this accepted assignment' })
    const original = page.locator('.delivery-notice').filter({ hasText: '群内派工未回复' })
    await expect(original).toBeVisible({ timeout: 15_000 })
    await expect(original.getByRole('button', { name: '重新尝试', exact: true })).toHaveCount(0)
    await original.getByRole('button', { name: '群内派工未回复', exact: true }).click()
    await expect(original.locator('.delivery-details')).toContainText('第 1 次投递')
    writeFileSync(script, JSON.stringify({ reply: 'retry completed' }))
    await original.getByRole('button', { name: '重新尝试', exact: true }).click()
    const retried = page.locator('.delivery-notice').filter({ hasText: '群内派工已完成' })
    await expect(retried).toBeVisible({ timeout: 15_000 })
    await expect(retried.getByRole('button')).toHaveAttribute('aria-expanded', 'false')
    await retried.getByRole('button').click()
    await expect(retried.locator('.delivery-details')).toContainText('第 2 次投递 · 重试投递')
    expect(await retried.innerText()).not.toContain('retry this accepted assignment')
    // 原投递不被改成成功，保留其未回复结果供追溯。
    await expect(original).toContainText('未回复')
  } finally {
    if (group) await api(page, 'deleteBot', { botId: group }).catch(() => {})
    if (member) await api(page, 'deleteBot', { botId: member }).catch(() => {})
    rmSync(cwd, { recursive: true, force: true })
    await browser.close()
  }
})

/** 长列表在首尾双向循环；连续按键、鼠标切换及单候选均不改变草稿或发送消息。 */
test('member arrows wrap repeatedly through a long list without losing steps', async () => {
  const { browser, page } = await attach()
  const prefix = `Loop ${Date.now()}`
  const ids: string[] = []
  let group: string | undefined
  try {
    for (let i = 0; i < 12; i++) ids.push((await steerBot(page, `${prefix} ${i.toString().padStart(2, '0')}`, 1)).id)
    group = (await api<{ bot: { id: string } }>(page, 'createBot', { name: prefix, kind: 'group', members: ids })).bot.id
    await page.getByRole('button', { name: prefix, exact: true }).click()
    const field = page.locator('.composer textarea')
    const menu = page.getByRole('listbox', { name: 'Members' })
    const rows = menu.getByRole('option')
    await field.fill('@')
    await expect(rows).toHaveCount(12)
    /** 每一步同时核验唯一高亮、可见性及编辑焦点，避免只验证最后的索引。 */
    const selected = async (index: number) => {
      await expect(menu.locator('[aria-selected="true"]')).toHaveCount(1)
      await expect(rows.nth(index)).toHaveAttribute('aria-selected', 'true')
      await expect(rows.nth(index)).toBeInViewport()
      await expect(field).toBeFocused()
      await expect(field).toHaveValue('@')
    }
    await selected(0)
    // 从第一项向上回到末项，再向下回到首项；随后各走完整两圈。
    let index = 0
    for (const key of ['ArrowUp', 'ArrowDown', ...Array<string>(24).fill('ArrowDown'), ...Array<string>(24).fill('ArrowUp')]) {
      await field.press(key)
      index = (index + (key === 'ArrowDown' ? 1 : 11)) % 12
      await selected(index)
    }
    // 同一帧的重复 keydown 覆盖按住方向键及渲染批处理场景。
    await field.evaluate((element) => {
      for (let i = 0; i < 7; i++) element.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', code: 'ArrowDown', repeat: i > 0, bubbles: true, cancelable: true }))
    })
    await selected(7)
    // 向上的批量重复键同样逐步累加，越过首项后继续从末项选择。
    await field.evaluate((element) => {
      for (let i = 0; i < 9; i++) element.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', code: 'ArrowUp', repeat: i > 0, bubbles: true, cancelable: true }))
    })
    await selected(10)
    const evidence = join(tmpdir(), 'codync-group-mentions-e2e')
    mkdirSync(evidence, { recursive: true })
    await page.screenshot({ path: join(evidence, 'member-keyboard-wrap.png') })
    // 鼠标停留在列表内时，键盘滚动也不能被悬停事件抢回高亮。
    await rows.nth(10).hover()
    await field.press('ArrowUp')
    await selected(9)
    await field.press('ArrowDown')
    await selected(10)
    await field.fill(`@${prefix} 03`)
    await expect(rows).toHaveCount(1)
    for (const key of ['ArrowUp', 'ArrowDown', 'ArrowUp', 'ArrowDown']) {
      await field.press(key)
      await expect(rows.nth(0)).toHaveAttribute('aria-selected', 'true')
    }
    await field.press('Tab')
    await expect(field).toHaveValue(`@${prefix} 03 `)
    // Tab 只插入，候选消失后的重复确认也不得误发。
    await field.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', repeat: true })
    await expect.poll(async () => (await api<History>(page, 'history', { botId: group })).entries.filter((entry) => entry.kind === 'user').length).toBe(0)
    await field.fill('@no-such-member')
    await expect(menu).not.toBeVisible()
  } finally {
    // 仅移除此用例创建的沙盒成员；清理失败不覆盖原始断言错误。
    if (group) await api(page, 'deleteBot', { botId: group }).catch(() => {})
    for (const id of ids) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    await browser.close()
  }
})

/** 在真实沙盒窗口验证编辑器身份、选人不发送，以及重载和改名后的请求正文。 */
test('group selection preserves caret text and stable identity through reload and rename', async () => {
  const { browser, page } = await attach()
  const suffix = Date.now()
  const memberName = `Mention ${suffix}`
  const groupName = `Mentions room ${suffix}`
  const evidence = join(tmpdir(), 'codync-group-mentions-e2e')
  mkdirSync(evidence, { recursive: true })
  const ids: string[] = []
  let groupId: string | undefined
  try {
    const first = await steerBot(page, memberName, 1)
    ids.push(first.id)
    const second = await steerBot(page, memberName, 1)
    ids.push(second.id)
    const group = await api<{ bot: { id: string } }>(page, 'createBot', { name: groupName, kind: 'group', members: ids })
    groupId = group.bot.id
    await page.locator('.roster-row', { hasText: groupName }).click()
    const field = page.locator('.composer textarea')
    const candidates = page.getByRole('listbox', { name: 'Members' })
    await field.fill('😀 @ tail')
    // 保留光标后正文，UTF-16 emoji 前缀必须计作两个单元。
    // 使用真实键盘移动触发 React 原生选区事件，单独派发 select 不能模拟其事件链。
    await field.press('Home')
    await field.press('ArrowRight')
    await field.press('ArrowRight')
    await field.press('ArrowRight')
    await expect(candidates.getByRole('option')).toHaveCount(2)
    await page.screenshot({ path: join(evidence, 'member-picker.png') })
    await field.press('ArrowDown')
    await expect(candidates.getByRole('option').nth(1)).toHaveAttribute('aria-selected', 'true')
    await field.press('ArrowUp')
    await expect(candidates.getByRole('option').nth(0)).toHaveAttribute('aria-selected', 'true')
    await field.press('Escape')
    await expect(candidates).not.toBeVisible()
    // 重新查询后用鼠标选择，点击不改变输入框的原选区。
    await field.press('ArrowLeft')
    await field.press('ArrowRight')
    await expect(candidates).toBeVisible()
    await candidates.getByRole('option').nth(0).click()
    await expect(field).toHaveValue(`😀 @${memberName}  tail`)
    await expect.poll(async () => (await api<History>(page, 'history', { botId: groupId })).entries.filter((e) => e.kind === 'user').length).toBe(0)

    // 替换草稿再以 Tab 选择另一成员，检验上下键与鼠标共用候选身份。
    await field.fill('😀 @ tail')
    // 使用真实键盘移动触发 React 原生选区事件，单独派发 select 不能模拟其事件链。
    await field.press('Home')
    await field.press('ArrowRight')
    await field.press('ArrowRight')
    await field.press('ArrowRight')
    await expect(candidates.getByRole('option')).toHaveCount(2)
    await field.press('ArrowDown')
    const selected = await candidates.getByRole('option').nth(1).innerText()
    const target = ids.find((id) => selected.includes(id.slice(0, 8)))!
    expect(target).toBeTruthy()
    // 模拟组合输入期间的确认键；真实中文输入法仍需人工设备验收。
    await field.dispatchEvent('compositionstart', { data: '中' })
    await field.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true, keyCode: 229 })
    await expect(field).toHaveValue('😀 @ tail')
    await field.dispatchEvent('compositionend', { data: '' })
    await field.press('Tab')
    await expect(field).toHaveValue(`😀 @${memberName}  tail`)
    await api(page, 'updateBot', { id: target, name: `Renamed ${suffix}` })
    await page.reload()
    await page.locator('.roster-row', { hasText: groupName }).click()
    await expect(field).toHaveValue(`😀 @${memberName}  tail`)
    await page.screenshot({ path: join(evidence, 'selected-draft-after-rename.png') })
    await field.press('End')
    await field.press('Enter')
    await expect.poll(async () => (await api<History>(page, 'history', { botId: groupId })).entries.find((e) => e.kind === 'user')?.data.mentions).toEqual([{ botId: target, start: 3, end: 4 + memberName.length }])
    await expect(field).toHaveValue('')
    await page.screenshot({ path: join(evidence, 'accepted-delivery-card.png') })
  } finally {
    if (groupId) await api(page, 'deleteBot', { botId: groupId }).catch(() => {})
    for (const id of ids) await api(page, 'deleteBot', { botId: id }).catch(() => {})
    await browser.close()
  }
})

/** 来源群必须直接展示成员保存故障，并能按成员身份恢复内存结果。 */
test('a source group exposes storage failure and retries the affected member save', async () => {
  const { browser, page } = await attach()
  const suffix = Date.now()
  const name = `Save member ${suffix}`
  const groupName = `Save room ${suffix}`
  const workspace = join(tmpdir(), `codync-mentions-save-${suffix}`)
  mkdirSync(workspace, { recursive: true })
  const database = new DatabaseSync(join(homedir(), `.codync-dev-${sandbox().name}`, 'codync.db'))
  let member: string | undefined
  let group: string | undefined
  try {
    const root = join(__dirname, '../../..')
    member = (await api<{ bot: { id: string } }>(page, 'createBot', {
      name, backend: 'custom', cwd: workspace, command: `python3 -X utf8 -u '${join(root, 'host/tests/fixtures/team_agent.py')}'`, connectors: [], skills: [], notify: false,
    })).bot.id
    group = (await api<{ bot: { id: string } }>(page, 'createBot', { name: groupName, kind: 'group', members: [member] })).bot.id
    await page.locator('.roster-row', { hasText: groupName }).click()
    await api(page, 'send', { botId: group, text: 'BLOCK group save acceptance' })
    // 始终只操作此 worktree 的沙盒数据库；制造结果写入失败后会在 finally 清理触发器。
    await expect.poll(async () => (await api<{ entries: { data: { deliveryRows?: { status: string }[] } }[] }>(page, 'history', { botId: group })).entries.some((e) => e.data.deliveryRows?.some((row) => row.status === 'started'))).toBe(true)
    database.exec("CREATE TRIGGER mentions_e2e_reject_result BEFORE INSERT ON entries WHEN NEW.kind='agent' BEGIN SELECT RAISE(ABORT,'e2e result storage failure'); END")
    writeFileSync(join(workspace, 'release'), '')
    const retry = page.getByRole('button', { name: '重试保存', exact: true })
    await expect(retry).toBeVisible()
    await expect(page.getByText(`${name}: 存储不可写`, { exact: false })).toBeVisible()
    const evidence = join(tmpdir(), 'codync-group-mentions-e2e')
    mkdirSync(evidence, { recursive: true })
    await page.screenshot({ path: join(evidence, 'source-group-storage-failure.png') })
    database.exec('DROP TRIGGER mentions_e2e_reject_result')
    await retry.click()
    await expect(retry).toHaveCount(0)
    await expect.poll(async () => (await api<{ entries: { data: { deliveryRows?: { status: string }[] } }[] }>(page, 'history', { botId: group })).entries.some((e) => e.data.deliveryRows?.some((row) => row.status === 'completed'))).toBe(true)
    // 已完成且无未保存结果时，群不能残留 working 状态或 Stop 按钮。
    await expect(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true })).toHaveCount(0)
    await page.screenshot({ path: join(evidence, 'source-group-storage-recovered.png') })
  } finally {
    database.exec('DROP TRIGGER IF EXISTS mentions_e2e_reject_result')
    database.close()
    if (group) await api(page, 'deleteBot', { botId: group }).catch(() => {})
    if (member) await api(page, 'deleteBot', { botId: member }).catch(() => {})
    await browser.close()
  }
})
