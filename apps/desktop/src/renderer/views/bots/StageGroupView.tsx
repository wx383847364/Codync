import { useEffect, useState } from 'react'
import type { Bot, StageChange, StageDetails, StageGroupDraft, StageUsage } from '@shared/models'
import { Button, Switch } from '../../components/Controls'
import { ModalHeader, SheetPage, useDismiss } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { useStore } from '../../store/context'
import { errorText } from './drafts'
import { AutoTextArea } from './parts'
import './bots.css'

/** 预览角色与项目目录，确认后创建独立阶段和首条草稿。 */
export function StageGroupView({ group }: { group: Bot }) {
  const store = useStore()
  const dismiss = useDismiss()
  const tr = useTr()
  const [requestId] = useState(() => crypto.randomUUID())
  const [name, setName] = useState('')
  const [goal, setGoal] = useState('')
  const [acceptance, setAcceptance] = useState('')
  const [leaderId, setLeader] = useState('')
  const [roles, setRoles] = useState<StageGroupDraft['roles']>([])
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const [handoffVersion, setHandoffVersion] = useState<string>()
  const handoff = group.stage?.handoff?.acceptedAt ? group.stage.handoff : undefined

  /** 在原名单中明确启用角色，取消时清除同一角色的负责人选择。 */
  function select(bot: Bot, on: boolean) {
    if (!on && leaderId === bot.id) setLeader('')
    setRoles(on ? [...roles, { botId: bot.id, sourceRev: bot.rev, description: bot.description, reuseProject: false }]
      : roles.filter((role) => role.botId !== bot.id))
  }

  /** 更新确认表中的角色字段，源机器人的资料由原聊天管理。 */
  function update(id: string, patch: Partial<StageGroupDraft['roles'][number]>) {
    setRoles(roles.map((role) => role.botId === id ? { ...role, ...patch } : role))
  }

  /** 断线后保留表单和创建 ID，重试可取回同一次提交。 */
  async function save() {
    setSaving(true)
    setError('')
    try {
      await store.createStageGroup({ botId: group.id, requestId, name, goal, acceptance, leaderId, handoffVersion,
        roles: group.members.flatMap((id) => roles.filter((role) => role.botId === id)) })
      dismiss()
    } catch (e) {
      setError(errorText(e))
    } finally {
      setSaving(false)
    }
  }

  return <SheetPage>
    <ModalHeader icon="person.2" title={tr('Create next stage group')} />
    <div className="group-scroll">
      <div className="group-form">
        <input className="field-box" value={name} placeholder={tr('Stage name')} aria-label={tr('Stage name')}
          onChange={(event) => setName(event.target.value)} />
        <AutoTextArea className="field-box" value={goal} placeholder="Stage goal" minRows={2} maxRows={5} onChange={setGoal} />
        <AutoTextArea className="field-box" value={acceptance} placeholder="Acceptance criteria" minRows={2} maxRows={5} onChange={setAcceptance} />
        {group.stage && (handoff
          ? <Switch label="Use accepted handoff" on={handoffVersion === handoff.file.id} disabled={saving}
            onChange={(on) => setHandoffVersion(on ? handoff.file.id : undefined)}>{tr('Use accepted handoff')} · {handoff.file.sha256.slice(0, 12)}</Switch>
          : <Text size="footnote" tone="secondary">{tr('Review and accept a handoff in Stage details first.')}</Text>)}
        <Text size="footnote" tone="secondary">{tr('New bots start with their own workspace and Codync shared memory off.')}</Text>
        <Text size="footnote" tone="secondary">{tr('Keep only stable instructions for the selected roles.')}</Text>
        {/* 名单顺序由来源群给出，用户仅确认此次沿用的角色内容。 */}
        {store.members(group).map((bot) => {
          const role = roles.find((candidate) => candidate.botId === bot.id)
          return <section key={bot.id}>
            <Switch label={bot.name} on={!!role} disabled={saving} onChange={(on) => select(bot, on)}>{bot.name}</Switch>
            {role && <>
              <Text as="div" size="caption" tone="secondary">{[bot.backend, bot.model].filter(Boolean).join(' · ')}</Text>
              <AutoTextArea className="field-box" value={role.description} placeholder="Role instructions" minRows={2} maxRows={6}
                onChange={(description) => update(bot.id, { description })} />
              {bot.cwd && !bot.managedWorkspace && <Switch label="Reuse project folder" on={role.reuseProject} disabled={saving}
                onChange={(reuseProject) => update(bot.id, { reuseProject })}>{tr('Reuse project folder')} · {bot.cwd}</Switch>}
              {bot.stageMemoryShared && <Text as="div" size="caption" tone="secondary">{tr('This agent shares its own memory across sessions.')}</Text>}
              <Button kind="secondary" disabled={saving || leaderId === bot.id} onClick={() => setLeader(bot.id)}>
                {leaderId === bot.id ? 'Stage lead' : 'Set as stage lead'}
              </Button>
            </>}
          </section>
        })}
        {error && <Text size="footnote" tone="danger">{error}</Text>}
        <Button disabled={saving || !leaderId || !name.trim() || !goal.trim() || !acceptance.trim() || (!!group.stage && !handoffVersion)} onClick={() => void save()}>
          {saving ? 'Creating…' : 'Create stage group'}
        </Button>
      </div>
    </div>
  </SheetPage>
}

/** 人工编辑 Markdown、核对执行记录，并分开验收与控制群准入。 */
export function StageDetailsView({ botId }: { botId: string }) {
  const store = useStore()
  const dismiss = useDismiss()
  const tr = useTr()
  const [details, setDetails] = useState<StageDetails>()
  const [markdown, setMarkdown] = useState('')
  const [evidence, setEvidence] = useState('')
  const [baseVersion, setBaseVersion] = useState<string>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  /** 加载最近已确认版本。失败时保留正在编辑的内容。 */
  async function load(change?: StageChange) {
    setBusy(true)
    setError('')
    try {
      const state = await store.stageDetails(botId, change)
      setDetails(state)
      if (!details || change?.action === 'saveHandoff') {
        setMarkdown(state.markdown ?? state.template)
        setEvidence(state.bot.stage?.handoff?.evidence ?? '')
        setBaseVersion(state.bot.stage?.handoff?.file.id)
      }
    } catch (e) {
      setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }
  // 一次打开读取一次持久版本；刷新按钮只更新运行状态。
  useEffect(() => { void load() }, [botId])

  const stage = details?.bot.stage
  const handoff = stage?.handoff
  const changed = markdown.trim() !== (details?.markdown ?? '').trim() || evidence.trim() !== (handoff?.evidence ?? '')
  const pending = details?.work.pending ?? []
  return <SheetPage>
    <ModalHeader icon="doc.text" title={tr('Stage details')} />
    <div className="group-scroll"><div className="group-form">
      {details && stage && <>
        <Text as="div" weight="semibold">{details.bot.name} · {tr(stage.closedAt ? 'Stage closed' : 'Stage open')}</Text>
        <Text as="div" size="footnote" tone="secondary">{tr(handoff?.acceptedAt ? 'Stage accepted' : 'Stage awaiting acceptance')}</Text>
        {Object.entries(details.work.counts).map(([status, count]) => <Text as="div" key={status} size="footnote">{tr(status)} · {count}</Text>)}
        {pending.map((item) => <Text as="div" key={item.id} size="footnote">{item.name} · {tr(item.status)}</Text>)}
        {details.incomingMarkdown && <details><summary>{tr('Incoming handoff')} · {stage.incoming?.sha256.slice(0, 12)}</summary>
          <pre style={{ whiteSpace: 'pre-wrap' }}>{details.incomingMarkdown}</pre></details>}
        {(details.handoffUnavailable || details.incomingUnavailable) && <Text tone="danger">{tr('The handoff file is unavailable.')}</Text>}
        {handoff && <Text size="caption" tone="secondary">{handoff.file.name} · {handoff.file.sha256.slice(0, 12)}</Text>}
        {stage.closedAt ? <>
          <pre style={{ whiteSpace: 'pre-wrap' }}>{details.markdown}</pre>
          {handoff?.evidence && <Text as="div">{handoff.evidence}</Text>}
        </> : <>
          <AutoTextArea className="field-box" value={markdown} placeholder="Handoff Markdown" minRows={7} maxRows={16} onChange={setMarkdown} disabled={busy} />
          <AutoTextArea className="field-box" value={evidence} placeholder="Durable evidence references" minRows={2} maxRows={5} onChange={setEvidence} disabled={busy} />
          {!!details.sharedFiles.length && <Text as="div" size="footnote">{tr('Shared evidence files')}</Text>}
          {details.sharedFiles.map((file) => <Button kind="secondary" key={file.id} disabled={busy || evidence.split('\n').includes(`file:${file.id}`)} onClick={() => {
            // 固定已分享的文件 ID，创建下一阶段时复制该版本。
            setEvidence([evidence.trimEnd(), `file:${file.id}`].filter(Boolean).join('\n'))
          }}><span>{file.name} · {file.sha256.slice(0, 12)}</span></Button>)}
          <Button kind="secondary" disabled={busy || !markdown.trim() || !changed} onClick={() => void load({ action: 'saveHandoff', markdown, evidence, previousVersion: baseVersion })}>Save handoff</Button>
        </>}
        {handoff && <Button kind="secondary" disabled={busy || changed || baseVersion !== handoff.file.id || !handoff.evidence || !!handoff.acceptedAt || !!pending.length || details.handoffUnavailable}
          onClick={() => void load({ action: 'accept', version: handoff.file.id })}>Accept stage</Button>}
        <Button kind="secondary" disabled={busy || (!stage.closedAt && !!pending.length)} onClick={() => void load({ action: stage.closedAt ? 'reopen' : 'close' })}>
          {stage.closedAt ? 'Reopen stage' : 'Close stage'}
        </Button>
        {!details.bot.lastMessage && !stage.closedAt && !store.composerDrafts.get(botId) && <Button kind="secondary" onClick={() => {
          store.composerDrafts.set(botId, details.initialDraft)
          store.setSelection(botId)
          dismiss()
        }}>Restore first draft</Button>}
      </>}
      <Button kind="secondary" disabled={busy} onClick={() => void load()}>Refresh</Button>
      {error && <Text size="footnote" tone="danger">{error}</Text>}
      {details && <StageUsageView botId={botId} />}
    </div></div>
  </SheetPage>
}

/** 按 Host 的分组展示观察计数，空值和只覆盖部分尝试的实报清晰区分。 */
function StageUsageView({ botId }: { botId: string }) {
  const store = useStore()
  const tr = useTr()
  const [usage, setUsage] = useState<StageUsage>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  /** 用户打开或刷新用量时再查本机数据。 */
  async function load() {
    setBusy(true)
    setError('')
    try { setUsage(await store.stageUsage(botId)) }
    catch (e) { setError(errorText(e)) }
    finally { setBusy(false) }
  }

  return <>
    <Button kind="secondary" disabled={busy} onClick={() => void load()}>{usage ? 'Refresh stage usage' : 'Stage usage'}</Button>
    {error && <Text size="footnote" tone="danger">{error}</Text>}
    {usage && <>
      <Text size="footnote" tone="secondary">{tr('Recording since {date}').replace('{date}', new Date(usage.observedSince).toLocaleDateString())}</Text>
      {usage.stageCreatedAt < usage.observedSince && <Text size="footnote" tone="secondary">{tr('Earlier attempts may be missing.')}</Text>}
      <Text size="footnote" tone="secondary">{tr('Reported fields may overlap. Account usage is separate.')}</Text>
      {usage.sections.map((section) => <section key={JSON.stringify([section.label, section.backend, section.model])}>
        <Text as="div" weight="semibold">{tr(section.label)}{section.backend ? ` · ${section.backend} · ${section.model ?? tr('Unknown model')}` : ''}</Text>
        {section.rows.map((row) => <Text as="div" key={row.label} size="footnote">
          {tr(row.label)} · {row.value?.toLocaleString() ?? tr('Unavailable')}{row.reports === undefined ? '' : ` (${row.reports}/${row.attempts})`}
        </Text>)}
      </section>)}
    </>}
  </>
}
