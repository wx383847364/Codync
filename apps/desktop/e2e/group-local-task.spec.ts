import { test, expect } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { SANDBOX_PREFIX } from '../src/shared/dev-sandbox.ts'
import { api, attach, sandbox, typeMessage } from './sandbox.ts'

type Bot = { id: string; name: string; rev: number }
type Entry = { id: string; botId: string; data: {text?: string; clientNonce?: string; deliveryRows?: {deliveryId: string; status: string; detail: string; resultId?: string}[];
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

test('Host tasks use the slash menu and exact aliases without prompting a new bot', async () => {
  // 可选的本机验收使用相同的交互，默认 CI 使用小型可运行脚本。
  test.setTimeout(180_000)
  const { browser, page } = await attach()
  const root = mkdtempSync(join(tmpdir(), 'codync-direct-task-e2e-'))
  const registry = join(homedir(), SANDBOX_PREFIX + sandbox().name, 'local-tasks.json')
  const previous = existsSync(registry) ? readFileSync(registry, 'utf8') : undefined
  const python = execFileSync(process.platform === 'win32' ? 'python' : 'python3',
    ['-c', 'import sys; print(sys.executable)'], { encoding: 'utf8' }).trim()
  const runner = join(root, 'direct-task.py')
  const actual = process.env.CODYNC_REAL_REGRESSION_RUNNER
  const approvedRepo = process.env.CODYNC_REAL_REGRESSION_REPO
  const title = actual ? '酒馆工作树五套软件回归' : 'Direct QA check'
  const alias = actual ? '跑一遍五套软件回归' : 'Run the direct QA fixture'
  const evidenceName = actual ? 'checks.json' : 'qa-evidence.txt'
  writeFileSync(runner, `
import json, os
from pathlib import Path
Path('qa-evidence.txt').write_text('direct fixture', encoding='utf-8')
Path(os.environ['CODYNC_TASK_RESULT']).write_text(json.dumps(
    {'status':'passed','summary':'The direct fixture finished','evidence':['qa-evidence.txt']}))
`)
  const rooms: string[] = []
  const bots: string[] = []
  try {
    const task = { title, program: python, args: [actual ?? runner, ...(approvedRepo ? ['--repo', approvedRepo] : [])], timeoutSeconds: 120, aliases: [alias] }
    writeFileSync(registry, JSON.stringify({ ...JSON.parse(previous ?? '{}'), 'direct-qa-e2e': task }))
    const {bot} = await api<{bot: Bot}>(page,'createBot',{
      name:'Direct QA no prompt', backend:'opencode', cwd:root, permission:'ask', connectors:[], skills:[], notify:false,
    })
    bots.push(bot.id)
    const {bot: regular} = await api<{bot: Bot}>(page,'createBot',{name:'Direct QA ordinary',kind:'group',members:[bot.id]})
    rooms.push(regular.id)
    const {bot: stage, createdMembers} = await api<{bot: Bot; createdMembers: string[]}>(page,'createStageGroup',{
      botId:regular.id, requestId:randomUUID(), name:'Direct QA stage', leaderId:bot.id,
      roles:[{botId:bot.id,sourceRev:bot.rev,description:''}],
    })
    rooms.push(stage.id)
    const command = `"${python}" -u "${join(__dirname,'../../../host/tests/fixtures/team_agent.py')}"`
    // 夹具只在收到模型提示时留下记录，新成员说明为空。
    for (const id of [bot.id, ...createdMembers]) await api(page,'updateBot',{id,backend:'custom',command,cwd:root})
    const evidenceIds: string[] = []
    for (const [room, menu] of [[regular,true],[stage,false]] as const) {
      await page.locator(`[data-roster-row$="/${room.id}"]`).click()
      if (menu) {
        await page.locator('.composer textarea').fill('/')
        await page.getByRole('option',{name:/Run task/}).click()
        await page.getByRole('option',{name:new RegExp(title)}).click()
      } else {
        await typeMessage(page, alias)
      }
      const notice = page.locator('.delivery-notice').last()
      await expect(notice).toContainText('Host execution', {timeout:15_000})
      const original = await notice.getAttribute('id')
      await notice.getByRole('button').first().click()
      await expect(notice).toContainText(/ · (Passed|Needs review|Blocked|Failed)/, {timeout:120_000})
      await expect(notice).toContainText(`${title} · Passed`)
      expect(await notice.getAttribute('id')).toBe(original)
      await notice.getByRole('button',{name:'View result'}).click()
      await expect(page.getByText(evidenceName,{exact:true})).toBeVisible()
      const {entries}=await api<{entries:Entry[]}>(page,'history',{botId:room.id})
      const resultId=entries.find(e=>e.id===original)?.data.deliveryRows?.[0]?.resultId
      expect(resultId).toBeTruthy()
      const {evidence}=await api<{evidence:Entry}>(page,'groupDeliveryResult',{roomId:room.id,resultId})
      const file=evidence.data.files?.find(f=>f.name===evidenceName)
      expect(file).toBeTruthy()
      const fetched=await api<{data:string}>(page,'readFile',{entryId:evidence.id,fileId:file?.id,offset:0})
      const body=Buffer.from(fetched.data,'base64').toString()
      if (actual) {
        const report=JSON.parse(body) as {suites:{passed:number}[]}
        expect(report.suites.map(s=>s.passed)).toEqual([279,19,17,2943,2894])
        expect(evidence.data.files?.map(f=>f.name).sort()).toEqual(['checks.json','regression-evidence.zip','snapshot.sha256','summary.md'])
      } else expect(body).toBe('direct fixture')
      evidenceIds.push(resultId!)
      if (process.env.CODYNC_TASK_EVIDENCE_DIR) await page.screenshot({path:join(process.env.CODYNC_TASK_EVIDENCE_DIR,`direct-${menu?'ordinary':'stage'}.png`)})
    }
    expect(existsSync(join(root,'prompts.jsonl'))).toBe(false)
    const stageUsage=await api<{counts:{attempts:number;prompts:number;sessions:number}; startedDeliveriesWithoutAttempt:number}>(page,'stageUsage',{botId:stage.id})
    expect(stageUsage.counts).toMatchObject({attempts:0,prompts:0,sessions:0})
    // Host 派工本身无需模型尝试，阶段用量也不应标为漏采。
    expect(stageUsage.startedDeliveriesWithoutAttempt).toBe(0)
    await expect(api(page,'groupDeliveryResult',{roomId:stage.id,resultId:evidenceIds[0]})).rejects.toThrow()
    // 同一提交编号只能重用同一个执行版本。
    const {items}=await api<{items:{taskId:string;version:string}[]}>(page,'groupLocalTasks',{roomId:regular.id})
    const item=items.find(row=>row.taskId==='direct-qa-e2e')!
    if (!actual) {
      const args={roomId:regular.id,...item,clientNonce:randomUUID()}
      const first=await api<{card:Entry}>(page,'runGroupLocalTask',args)
      const next=await api<{card:Entry}>(page,'runGroupLocalTask',args)
      expect(next.card.id).toBe(first.card.id)
      await expect(api(page,'runGroupLocalTask',{...args,version:'wrong'})).rejects.toThrow()
      // 单执行位被占用时保留原卡，登记版本变化会阻塞排队任务。
      const slowRelease = join(root,'slow-release')
      const slowRunner = join(root,'slow.py')
      writeFileSync(slowRunner,`
import sys,time
from pathlib import Path
release=Path(sys.argv[1])
release.with_suffix('.pid').write_text(str(__import__('os').getpid()))
while not release.exists(): time.sleep(.05)
`)
      const definitions = { ...JSON.parse(readFileSync(registry,'utf8')),
        'direct-slow': { title:'Slow Host task', program:python, args:[slowRunner,slowRelease],timeoutSeconds:65 } }
      writeFileSync(registry,JSON.stringify(definitions))
      const catalog=await api<{items:{taskId:string;version:string}[]}>(page,'groupLocalTasks',{roomId:regular.id})
      const slow=catalog.items.find(task=>task.taskId==='direct-slow')!
      await api(page,'runGroupLocalTask',{roomId:regular.id,...slow,clientNonce:randomUUID()})
      await expect.poll(()=>existsSync(join(root,'slow-release.pid')),{timeout:12_000}).toBe(true)
      const pid=Number(readFileSync(join(root,'slow-release.pid'),'utf8'))
      const queued=await api<{card:Entry}>(page,'runGroupLocalTask',{roomId:stage.id,...item,clientNonce:randomUUID()})
      expect(queued.card.data.deliveryRows?.[0]?.status).toBe('queued')
      definitions['direct-qa-e2e'].title='Replaced registration'
      writeFileSync(registry,JSON.stringify(definitions))
      await api(page,'stop',{botId:regular.id})
      await expect.poll(()=>{
        try { process.kill(pid,0); return true } catch { return false }
      }).toBe(false)
      await expect.poll(async()=>{
        const {entries}=await api<{entries:Entry[]}>(page,'history',{botId:stage.id})
        return entries.find(entry=>entry.id===queued.card.id)?.data.deliveryRows?.[0]?.detail
      },{timeout:12_000}).toContain('Blocked')
      definitions['ambiguous-e2e']={...definitions['direct-qa-e2e'],title:'Another matching task'}
      writeFileSync(registry,JSON.stringify(definitions))
      const ambiguousNonce = randomUUID()
      await expect(api(page,'send',{botId:stage.id,text:alias,clientNonce:ambiguousNonce})).rejects.toThrow()
      const {entries: afterAmbiguous}=await api<{entries:Entry[]}>(page,'history',{botId:stage.id})
      expect(afterAmbiguous.some(e=>e.data.clientNonce===ambiguousNonce)).toBe(false)
      expect(existsSync(join(root,'prompts.jsonl'))).toBe(false)
    }
  } finally {
    for (const id of rooms.reverse()) await api(page,'purgeGroup',{botId:id}).catch(() => {})
    for (const id of bots) await api(page,'deleteBot',{botId:id}).catch(() => {})
    if (previous == null) rmSync(registry,{force:true})
    else writeFileSync(registry,previous)
    rmSync(root,{recursive:true,force:true})
    await browser.close()
  }
})
