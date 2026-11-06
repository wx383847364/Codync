import { test, expect } from '@playwright/test'
import { mkdirSync, writeFileSync } from 'node:fs'
import { DatabaseSync } from 'node:sqlite'
import { join } from 'node:path'
import { homedir, tmpdir } from 'node:os'
import { api, attach, sandbox, steerBot } from './sandbox.ts'

type History = { entries: { kind: string; data: { text?: string; mentions?: { botId: string; start: number; end: number }[] } }[] }

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
