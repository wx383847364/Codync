import { useEffect, useState } from 'react'
import type { Bot, StageChange, StageDetails, StageGroupDraft, StageUsage } from '@shared/models'
import { GroupAvatar } from '../../components/Avatar'
import { Button, Switch } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, Sheet, SheetPage, useDismiss } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { useStore } from '../../store/context'
import { errorText } from './drafts'
import { AutoTextArea } from './parts'
import { CreateLayout } from './NewBotView'
import { GroupBotPicker, GroupMemberPicker } from './GroupMemberPicker'
import './bots.css'

/** 预览角色与项目目录，确认后创建独立阶段和首条草稿。 */
export function StageGroupView({ group }: { group: Bot }) {
  const store = useStore()
  const tr = useTr()
  const [requestId] = useState(() => crypto.randomUUID())
  const [name, setName] = useState('')
  const [goal, setGoal] = useState('')
  const [acceptance, setAcceptance] = useState('')
  const [leaderId, setLeader] = useState<string>()
  const [roles, setRoles] = useState<StageGroupDraft['roles']>([])
  const [roleEditor, setRoleEditor] = useState<string>()
  const [handoffVersion, setHandoffVersion] = useState<string>()
  const handoff = group.stage?.handoff?.acceptedAt ? group.stage.handoff : undefined
  const bots = store.members(group)
  const picked = bots.filter((bot) => roles.some((role) => role.botId === bot.id))
  const editingBot = picked.find((bot) => bot.id === roleEditor)
  const editingRole = roles.find((role) => role.botId === editingBot?.id)
  const leader = picked.find((bot) => bot.id === leaderId)
  const accepted = handoff && handoffVersion === handoff.file.id
  const optional = store.hasGroupMemberPicker

  /** 只复制来源群已选角色，移除角色时同步清空其负责人选择。 */
  function select(bot: Bot) {
    if (roles.some((role) => role.botId === bot.id)) {
      if (leaderId === bot.id) setLeader(undefined)
      setRoles(roles.filter((role) => role.botId !== bot.id))
    } else {
      setRoles([...roles, { botId: bot.id, sourceRev: bot.rev, description: bot.description, reuseProject: false }])
    }
  }

  /** 更新确认表中的角色字段，源机器人的资料由原聊天管理。 */
  function update(id: string, patch: Partial<StageGroupDraft['roles'][number]>) {
    setRoles(roles.map((role) => role.botId === id ? { ...role, ...patch } : role))
  }

  /** 固定来源成员顺序与创建 ID，连接失败时由共用表单保留输入。 */
  async function save() {
    if (!leader) return
    await store.createStageGroup({
      botId: group.id, requestId, name, goal, acceptance, leaderId: leader.id,
      handoffVersion: accepted ? handoff.file.id : undefined,
      roles: group.members.flatMap((id) => roles.filter((role) => role.botId === id)),
    })
  }

  return <>
    <CreateLayout title="Create next stage group" group
      preview={<GroupAvatar members={picked} size={80} />}
      name={name} placeholder="Stage name" onName={setName} create={save}
      canCreate={!!leader && !!name.trim() && (optional || (!!goal.trim() && !!acceptance.trim())) && (!group.stage || !!accepted)}>
      <GroupMemberPicker bots={bots} picked={picked} onToggle={select} />
      <GroupBotPicker bots={picked} selected={leader?.id} onChange={setLeader} label="Group lead" />
      <AutoTextArea className="field-box" value={goal} placeholder={optional ? 'Stage goal (optional)' : 'Stage goal'} minRows={2} maxRows={5} onChange={setGoal} />
      <AutoTextArea className="field-box" value={acceptance} placeholder={optional ? 'Acceptance criteria (optional)' : 'Acceptance criteria'} minRows={2} maxRows={5} onChange={setAcceptance} />
      {group.stage && (handoff
        ? <div className="stage-handoff-choice">
          <Switch label="Use accepted handoff" on={!!accepted}
            onChange={(on) => setHandoffVersion(on ? handoff.file.id : undefined)}>{tr('Use accepted handoff')} · {handoff.file.sha256.slice(0, 12)}</Switch>
        </div>
        : <Text as="div" size="footnote" tone="secondary" className="create-group-note">{tr('Review and accept a handoff in Stage details first.')}</Text>)}
      <button type="button" className="field-box stage-role-settings" disabled={!picked.length}
        onClick={() => setRoleEditor(picked[0]?.id)}>
        {tr('Role settings')}
        <Icon name="chevron.right" size={12} />
      </button>
    </CreateLayout>
    <Sheet open={!!editingBot && !!editingRole} onClose={() => setRoleEditor(undefined)} width={400}>
      {editingBot && editingRole && <SheetPage>
        <ModalHeader title={tr('Role settings')} />
        <div className="group-scroll">
          <div className="stage-role-form">
            <GroupBotPicker bots={picked} selected={editingBot.id} onChange={setRoleEditor} label="Role settings" showsLabel={false} />
            <Text as="div" size="caption" tone="secondary">{[editingBot.backend, editingBot.model].filter(Boolean).join(' · ')}</Text>
            <Text as="div" size="footnote" tone="secondary">{tr('Keep only stable instructions for the selected roles.')}</Text>
            <AutoTextArea className="field-box" value={editingRole.description} placeholder="Role instructions" minRows={3} maxRows={8}
              onChange={(description) => update(editingBot.id, { description })} />
            {editingBot.cwd && !editingBot.managedWorkspace &&
              <Switch label="Reuse project folder" on={!!editingRole.reuseProject}
                onChange={(reuseProject) => update(editingBot.id, { reuseProject })}>{tr('Reuse project folder')} · {editingBot.cwd}</Switch>}
            <Text as="div" size="footnote" tone="secondary">{tr('New bots start with their own workspace and Codync shared memory off.')}</Text>
            {editingBot.stageMemoryShared &&
              <Text as="div" size="caption" tone="secondary">{tr('This agent shares its own memory across sessions.')}</Text>}
            <Button onClick={() => setRoleEditor(undefined)}>Done</Button>
          </div>
        </div>
      </SheetPage>}
    </Sheet>
  </>
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
        {!!details.initialDraft && !details.bot.lastMessage && !stage.closedAt && !store.composerDrafts.get(botId) && <Button kind="secondary" onClick={() => {
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
