import { useState, type ReactNode } from 'react'
import { isGroup, type Bot } from '@shared/models'
import { CharacterAvatar, GroupAvatar } from '../../components/Avatar'
import { Button, ChoicePicker, Spinner } from '../../components/Controls'
import { ModalHeader, useDismiss } from '../../components/Overlay'
import { HStack, Spacer, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { useTr } from '../../lib/use-language'
import { errorText, fillDefaults, newDraft } from './drafts'
import { AvatarPicker } from './parts'
import { GroupBotPicker, GroupMemberPicker } from './GroupMemberPicker'
import './bots.css'

/** Which computer a new bot or group goes to; shown when more than one is online. */
export interface ComputerChoice {
  options: { id: string; label: string }[]
  selection: string
  onChange: (id: string) => void
}

/**
 * A new bot from its looks and an optional name (Grok Bot's "Create New Bot"); the rest starts on
 * the defaults and changes in its Settings. Unnamed, the host names it after its first conversations.
 */
export function NewBotView({ computer }: { computer: ComputerChoice | null }) {
  const store = useStore()
  const [draft, setDraft] = useState(newDraft)

  const create = async () => {
    const bot = await store.save(fillDefaults(store, { ...draft, name: draft.name.trim() }))
    store.setSelection(bot.id)
  }

  return (
    <CreateLayout
      title="New bot"
      preview={<CharacterAvatar shape={draft.avatarShape} color={draft.avatarColor} size={96} />}
      name={draft.name}
      placeholder="Name your bot"
      onName={(name) => setDraft((d) => ({ ...d, name }))}
      canCreate
      computer={computer}
      create={create}
    >
      <AvatarPicker
        shape={draft.avatarShape}
        color={draft.avatarColor}
        onShape={(avatarShape) => setDraft((d) => ({ ...d, avatarShape }))}
        onColor={(avatarColor) => setDraft((d) => ({ ...d, avatarColor }))}
      />
    </CreateLayout>
  )
}

/** 普通群与空白新会话使用同一套建群表单，来源群只用于初始化。 */
export function NewGroupView({ computer = null, source }: { computer?: ComputerChoice | null; source?: Bot }) {
  const tr = useTr()
  const store = useStore()
  const [name, setName] = useState('')
  const [members, setMembers] = useState<string[]>(() => source?.members ?? [])
  const [leader, setLeader] = useState(source?.stage?.leaderId ?? source?.groupLeaderId)
  const visible = store.roster.filter((bot) => !isGroup(bot))
  const picked = members.map((id) => store.bots.get(id)).filter((bot): bot is Bot => !!bot && !isGroup(bot))
  const visibleIds = new Set(visible.map((bot) => bot.id))
  // 原群中的隐藏成员移除后仍可重新加入本次新会话。
  const inherited = source ? store.members(source) : []
  const candidates = [...visible, ...inherited.filter((bot) => !visibleIds.has(bot.id))]
  const selectedLeader = picked.find((bot) => bot.id === leader)
  const defaultName = picked.map((bot) => bot.name).join(', ')

  /** 移除负责人时同步清空选择，避免提交无效的成员 ID。 */
  const toggle = (bot: Bot) => {
    if (members.includes(bot.id)) {
      setMembers(members.filter((id) => id !== bot.id))
      if (leader === bot.id) setLeader(undefined)
    } else {
      setMembers([...members, bot.id])
    }
  }

  /** 新会话由 Host 分配编号，普通群按输入名称或成员名称创建。 */
  const create = async () => {
    const ids = picked.map((bot) => bot.id)
    if (source) await store.saveGroupSession(source.id, { members: ids, groupLeaderId: selectedLeader?.id })
    else await store.createGroup(name.trim() || defaultName, '', ids, store.hasGroupMemberPicker ? selectedLeader?.id : undefined)
  }

  return (
    <CreateLayout
      title={source ? 'Create new group session' : 'New group chat'}
      preview={<GroupAvatar members={picked} size={80} />}
      name={source ? tr('{name} · Auto-numbered').replace('{name}', () => source.name) : name}
      placeholder={defaultName || 'Name your group'}
      onName={source ? undefined : setName}
      canCreate={picked.length > 0}
      computer={computer}
      create={create}
      group
    >
      <GroupMemberPicker bots={candidates} picked={picked} onToggle={toggle} />
      {store.hasGroupMemberPicker && <GroupBotPicker bots={picked} selected={selectedLeader?.id} onChange={setLeader} label="Group lead (optional)" allowsNone />}
      <Text as="div" size="caption" tone="tertiary" className="create-group-note">
        {source ? tr('Original members are preselected. You can change them.')
          : selectedLeader ? tr('Ask {name} · @ to choose another recipient').replace('{name}', () => selectedLeader.name)
          : tr('Ask everyone · @ to ask one bot')}
      </Text>
    </CreateLayout>
  )
}

/** 建群与新建机器人共用预览、名称、创建按钮与失败反馈。 */
export function CreateLayout({ title, preview, name, placeholder, onName, canCreate, computer, create, children, group = false }: {
  title: string
  preview: ReactNode
  name: string
  placeholder: string
  onName?: (name: string) => void
  canCreate: boolean
  computer?: ComputerChoice | null
  group?: boolean
  create: () => Promise<void>
  children: ReactNode
}) {
  const store = useStore()
  const tr = useTr()
  const dismiss = useDismiss()
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const ready = canCreate && !saving && store.connection.kind === 'online'

  /** 仅在表单有效且未提交时发送创建请求。 */
  const submit = () => {
    if (!ready) return
    setSaving(true)
    setError(null)
    create()
      .then(dismiss)
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setSaving(false))
  }

  return (
    <VStack className={group ? "create-sheet create-group-sheet" : "create-sheet"}>
      <ModalHeader title={tr(title)} />
      <VStack align="center" gap={group ? 12 : 20} className="create-body">
        {preview}
        <input
          className="field-box create-name"
          autoFocus={!!onName}
          readOnly={!onName}
          value={name}
          placeholder={tr(placeholder)}
          aria-label={tr(group && placeholder === "Stage name" ? "Stage name" : "Name")}
          onChange={(e) => onName?.(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.nativeEvent.isComposing && e.keyCode !== 229) submit()
          }}
        />
        {children}
        {error ? <Text size="footnote" tone="danger" className="fade-in">{error}</Text> : null}
      </VStack>
      <HStack gap={8} className="create-footer">
        {computer ? (
          <HStack gap={8}>
            <Text tone="secondary">{tr(`On`)}</Text>
            <ChoicePicker selection={computer.selection} options={computer.options} onChange={computer.onChange} />
          </HStack>
        ) : null}
        <Spacer />
        <Button kind="secondary" onClick={dismiss}>Cancel</Button>
        <Button onClick={submit} disabled={!ready}>{saving ? <Spinner /> : 'Create'}</Button>
      </HStack>
    </VStack>
  )
}
