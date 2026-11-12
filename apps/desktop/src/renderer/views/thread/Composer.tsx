import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react'
import { groupFastMode } from '@shared/fast-mode'
import { artifactDraftKey, artifactQuote, type Artifact } from '@shared/artifacts'
import { isGroup, isWorkingIn, quoteOf, type Entry } from '@shared/models'
import type { PickedFile } from '@shared/ipc'
import { useMentionMenu } from './MentionMenu'
import { Icon } from '../../components/Icon'
import { IconButton } from '../../components/Controls'
import { RecordingPill } from '../../components/RecordingPill'
import { Sheet, usePresence } from '../../components/Overlay'
import { VoiceChatSettingsView } from '../settings/VoiceChatSettingsView'
import { HStack, VStack } from '../../components/Stack'
import { useModel } from '../../lib/observable'
import { useTr } from '../../lib/use-language'
import { useStore } from '../../store/context'
import { type OutgoingFile } from '../../store/bot-store'
import { pluginsFor } from '../marketplace/plugins'
import { pastedFiles, readPicked } from './composer-files'
import { composerEnter } from './composer-keyboard'
import { CommandMenu, MentionMenu, useComposerMenu } from './ComposerMenu'
import { ComposerAttachment } from './ComposerAttachment'
import { QuoteRow } from './ChatRows'
import { useDictation } from './dictation-platform'
import { DictationEngineMenu } from './DictationEngineMenu'
import { builtInCommand, returnRuns, slashCommands, slashQuery, type SlashCommand } from './slash-commands'
import { useVoiceShortcut } from './voice-shortcut-keys'

type Trailing = 'interrupt' | 'stop' | 'call' | 'send'

/**
 * The message box under a chat: send, or stop while the bot (or the group) is working there. A leading `/` lists
 * the commands (a group's are its bots'); in a group, typing `@` lists its bots. While `reply` is set, the
 * message it quotes sits at the top of the box and goes out with the next send. Under an artifact, `about` is the
 * artifact: the box keeps a draft of its own, and every message replies to the artifact's card.
 */
export function Composer({ botId, reply = null, about = null, cancelReply, onCall, onInterrupt }: {
  botId: string
  reply?: Entry | null
  about?: Artifact | null
  cancelReply?: () => void
  /** Starts a voice call; while the box is empty it takes the send button's place. */
  onCall?: (() => void) | null
  /** Interrupts the bot while it is speaking in a voice call. */
  onInterrupt?: (() => void) | null
}) {
  const store = useStore()
  // Remount transient editor state (selection, attachments, IME) when its destination changes.
  return <DestinationComposer key={JSON.stringify([store.contextId, store.computer.id, botId, about?.id])}
    botId={botId} reply={reply} about={about} cancelReply={cancelReply} onCall={onCall} onInterrupt={onInterrupt} />
}

/** 管理单一目标编辑器，选人和发送共享同一草稿身份。 */
function DestinationComposer({ botId, reply, about, cancelReply, onCall, onInterrupt }: {
  botId: string; reply: Entry | null; about: Artifact | null; cancelReply?: () => void; onCall?: (() => void) | null; onInterrupt?: (() => void) | null
}) {
  const store = useStore()
  const tr = useTr()
  const plugins = useModel(pluginsFor(store))
  const bot = store.bots.get(botId) ?? null
  const drafts = store.composerDrafts
  const draftKey = about ? artifactDraftKey(about) : botId
  const draft = useSyncExternalStore(drafts.subscribe, () => drafts.get(draftKey))
  /** 提交一次原生编辑范围，正文和绑定同时更新。 */
  const setDraft = (text: string, replacement?: { start: number; end: number }) => drafts.set(draftKey, text, replacement)
  const [submitting, setSubmitting] = useState(false)
  const [files, setFiles] = useState<OutgoingFile[]>([])
  const [voiceSettings, setVoiceSettings] = useState(false)
  const dictation = useDictation((change) => drafts.set(draftKey, change(drafts.get(draftKey))), () => setVoiceSettings(true))
  const field = useRef<HTMLTextAreaElement>(null)
  const measure = useRef<HTMLDivElement>(null)
  const composing = useRef(false)
  const replacement = useRef<{ start: number; end: number } | undefined>(undefined)
  /** Text pasted into this draft since the last send. */
  const pasted = useRef<string[]>([])
  const working = !!bot && isWorkingIn(bot, botId)
  const empty = !draft.trim() && !files.length
  const canSend = !submitting && !empty && (store.connection.kind === 'online' || (store.canQueue && !files.length))
  const canAttach = !!bot

  const members = bot && isGroup(bot) ? store.members(bot) : []
  // 来源群显示每个成员的内存保存故障，重试直接指向实际持有结果的 actor。
  const storageFailures = (bot && isGroup(bot) ? members.filter((member) => member.workingChat === botId) : bot ? [bot] : []).filter((member) => member.storageError)

  const name = bot?.name ?? ''
  const placeholder = about
    ? tr('Message about “{title}”').replace('{title}', about.title)
    : reply
    ? tr('Reply…')
    : bot && isGroup(bot)
      ? tr('Ask everyone · @ to ask one bot')
      : working
        ? tr('Queue a message for {name}').replace('{name}', name)
        : tr('Ask {name}').replace('{name}', name)

  // Picking a message to reply to brings the caret here.
  useEffect(() => {
    if (reply) field.current?.focus()
  }, [reply])

  // Measure a separate, wrapping copy: collapsing the live textarea to zero breaks its
  // height transition, caret scroll and IME composition. Observe font/width changes too.
  useLayoutEffect(() => {
    const el = field.current
    const mirror = measure.current
    if (!el || !mirror) return
    const resize = () => {
      const style = getComputedStyle(el)
      const line = parseFloat(style.lineHeight)
      const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom)
      const height = Math.min(mirror.getBoundingClientRect().height, line * 8 + padding)
      el.style.height = `${height}px`
    }
    resize()
    const observer = new ResizeObserver(resize)
    observer.observe(mirror)
    return () => observer.disconnect()
  }, [])

  // 在原光标范围内插入，保留前后正文及选中成员身份。
  const mentionMenu = useMentionMenu(draft, members, (member, start, end) => {
    const token = `@${member.name}`
    const next = `${draft.slice(0, start)}${token} ${draft.slice(end)}`
    drafts.selectMention(draftKey, next, { botId: member.id, start, end: start + token.length }, { start, end })
    const caret = start + token.length + 1
    // 选择只插入草稿，下一帧恢复原输入焦点并把光标放在成员片段后。
    requestAnimationFrame(() => {
      // 上次选择的帧回调不得覆盖用户随后输入的新草稿或移动其光标。
      if (drafts.get(draftKey) !== next) return
      field.current?.focus()
      field.current?.setSelectionRange(caret, caret)
      mentionMenu.selection({ start: caret, end: caret })
    })
  })

  const slash = slashQuery(draft)
  const commands = slash === null ? [] : slashCommands(bot, slash, plugins.installedSkills, members)

  /** 执行快捷命令，群会话切换按成员意图交给 host。 */
  const run = (command: SlashCommand) => {
    if (!bot) return
    switch (command.action.kind) {
      case 'fast':
        if (isGroup(bot)) {
          // Each member's own Fast mode: all on, or all back off.
          const fast = groupFastMode(members)
          for (const m of fast?.members ?? []) store.setFastMode(m, fast?.state === 'off')
        } else {
          store.setFastMode(bot, !bot.fastMode)
        }
        setDraft('')
        break
      case 'session':
        void store.switchSession(botId, command.action.mode)
        setDraft('')
        break
      case 'skill':
        setDraft(`/${command.action.id} `)
        break
    }
    field.current?.focus()
  }
  const commandMenu = useComposerMenu(draft, commands, run, returnRuns)

  /** 接收所选附件，不按总大小拒绝；发送仍由既有分片协议完成。 */
  const append = (incoming: PickedFile[]) => {
    if (!canAttach) return
    const accepted: OutgoingFile[] = []
    for (const f of incoming) {
      accepted.push({ id: crypto.randomUUID(), name: f.name, data: f.data })
    }
    setFiles((list) => [...list, ...accepted])
  }

  // host 拒绝提及时保留草稿，确认受理后才清空当前文本。
  const submit = async () => {
    const command = files.length ? null : builtInCommand(bot, draft, members)
    if (!command && !canSend) return
    // Words still on their way would land in the emptied box (and the shortcut's tap would send them again).
    dictation.cancel()
    if (command) return run(command)
    setSubmitting(true)
    const sentMentions = drafts.mentions(draftKey)
    const sentFiles = new Set(files.map((file) => file.id))
    const accepted = await store.send(draft, botId, { files, mentions: sentMentions, pasted: pasted.current, replyTo: reply ? quoteOf(reply) : about && artifactQuote(about) })
    setSubmitting(false)
    if (!accepted) return
    cancelReply?.()
    pasted.current = []
    // 上传期间新加的附件和重新选择的同名成员仍属于下一份草稿。
    if (drafts.get(draftKey) === draft && JSON.stringify(drafts.mentions(draftKey)) === JSON.stringify(sentMentions)) setDraft('')
    setFiles((current) => current.filter((file) => !sentFiles.has(file.id)))
    field.current?.focus()
  }
  useVoiceShortcut(dictation, draft, submit, () => field.current?.focus())

  const trailing: Trailing = onInterrupt && !draft && !files.length ? 'interrupt' : working && !draft && !files.length ? 'stop' : onCall && !draft && !files.length ? 'call' : 'send'
  const [symbol, label] = ({
    interrupt: ['stop.fill', 'Interrupt'],
    stop: ['stop.fill', tr('Stop')],
    call: ['waveform', tr('Call')],
    send: ['arrow.up', 'Send'],
  } as const)[trailing]
  const enabled = trailing !== 'send' || canSend

  return (
    <VStack>
      {storageFailures.map((member) => <HStack key={member.id} gap={8}><span>{member.name}: {member.storageError}</span>
        {/* 故障入口不依赖尚未写出的投递卡片，使用成员身份恢复保存。 */}
        <button className="press" onClick={() => store.retryDeliverySave(member.id)}>重试保存</button>
      </HStack>)}
      {bot?.pendingNewSession ? <span>将在当前任务结束后新建会话；成员主会话与普通群共享。输入 /new-cancel 取消切换。</span> : null}
      <div
        className="composer"
        onDragOver={(e) => {
          if (canAttach && e.dataTransfer.types.includes('Files')) e.preventDefault()
        }}
        onDrop={(e) => {
          if (!canAttach) return
          e.preventDefault()
          void readPicked([...e.dataTransfer.files]).then(append)
        }}
      >
        <div className="composer-surface">
          <MentionMenu menu={{ ...mentionMenu, pick: mentionMenu.choose }} />
          <CommandMenu menu={commandMenu} />
          <ComposerReveal open={!!reply}>
            {reply ? (
              <HStack gap={4} className="composer-reply">
                <QuoteRow quote={quoteOf(reply)} className="composer-reply-quote" />
                <IconButton title="Cancel reply" icon="xmark" size={24} onClick={cancelReply} />
              </HStack>
            ) : null}
          </ComposerReveal>
          <ComposerReveal open={files.length > 0}>
            <div className="file-chips">
              {files.map((f) => (
                <ComposerAttachment key={f.id} file={f} remove={() => setFiles((list) => list.filter((x) => x.id !== f.id))} />
              ))}
            </div>
          </ComposerReveal>
          <div className="composer-box">
            {/* One capsule: attachments, the field, and send / stop / call. */}
            {canAttach ? (
              <button className="press composer-add" aria-label="Add files" title="Add files" onClick={() => void window.codync.app.pickFiles().then(append)}>
                <Icon name="plus" size={13} weight="medium" color="var(--text)" />
              </button>
            ) : null}
            <div className="composer-field">
              <div ref={measure} className="composer-measure" aria-hidden="true">{draft + '\u200b'}</div>
              <textarea
                ref={field}
                rows={1}
                value={draft}
                placeholder={placeholder}
                spellCheck
                aria-label={tr(reply ? 'Reply' : 'Message')}
                aria-keyshortcuts="Enter Shift+Enter"
                onCompositionStart={() => { composing.current = true }}
                onCompositionEnd={() => { composing.current = false }}
                // 光标变化也刷新候选，支持正文中间插入。
                onSelect={(e) => mentionMenu.selection({ start: e.currentTarget.selectionStart, end: e.currentTarget.selectionEnd })}
                // 保存真实替换选区，整段粘贴与重复字符编辑都解除被触及的绑定。
                onBeforeInput={(event) => { replacement.current = { start: event.currentTarget.selectionStart, end: event.currentTarget.selectionEnd } }}
                // 原生替换范围随一次正文更新消费，后续编辑不能复用旧范围。
                onChange={(e) => {
                  mentionMenu.selection({ start: e.target.selectionStart, end: e.target.selectionEnd })
                  setDraft(e.target.value, replacement.current)
                  replacement.current = undefined
                  // Starting a command: list the computer's skills again.
                  if (bot && e.target.value === '/') void plugins.refresh()
                }}
                // 候选和组合输入先处理确认键，之后才允许执行发送快捷键。
                onKeyDown={(e) => {
                  if (mentionMenu.onKey(e, composing.current || e.nativeEvent.isComposing || e.keyCode === 229)) return
                  if (commandMenu.onKey(e, composing.current || e.nativeEvent.isComposing || e.keyCode === 229)) return
                  if (e.key === 'Escape' && reply) {
                    e.preventDefault()
                    cancelReply?.()
                    return
                  }
                  // Return sends, Shift-Return adds a line; an input method's Return picks its candidate.
                  const action = composerEnter({ key: e.key, shiftKey: e.shiftKey, altKey: e.altKey, repeat: e.repeat, isComposing: composing.current || e.nativeEvent.isComposing, keyCode: e.keyCode })
                  if (e.key === 'Enter' && e.repeat && !e.shiftKey && !e.altKey) e.preventDefault()
                  if (action === 'send') {
                    e.preventDefault()
                    submit()
                  }
                }}
                onPaste={(e) => {
                  const types = [...e.clipboardData.types]
                  const hasFiles = types.includes('Files')
                  // Pasted text is sent marked as pasted: memory never takes it as the user's own words.
                  if (types.includes('text/plain') && !e.clipboardData.files.length) pasted.current.push(e.clipboardData.getData('text/plain'))
                  // ⌘V with a copied file or image: attach it (the field alone would paste a name).
                  if (!canAttach) return
                  if (!hasFiles || (types.includes('text/plain') && !e.clipboardData.files.length)) return
                  e.preventDefault()
                  void pastedFiles(window.codync.app.readClipboardFiles, [...e.clipboardData.files]).then(append)
                }}
              />
            </div>
            {dictation.listening || dictation.transcribing ? (
              <RecordingPill
                startedAt={dictation.startedAt}
                sample={() => dictation}
                onStop={() => dictation.finish()}
                transcribing={dictation.transcribing}
                className="composer-recording"
              />
            ) : dictation.available ? (
              <HStack as="span" gap={0} className="composer-mic-group">
                <button className="press composer-mic" aria-label="Dictate" title="Dictate" onClick={() => dictation.start(draft)}>
                  <Icon name="mic" size={13} weight="medium" />
                </button>
                <DictationEngineMenu />
              </HStack>
            ) : null}
            <button
              className={trailing === 'interrupt' ? 'press composer-send interrupt' : 'press composer-send'}
              disabled={!enabled}
              aria-label={label}
              title={label}
              onClick={() => {
                if (trailing === 'interrupt') onInterrupt?.()
                else if (trailing === 'stop') store.stop(botId)
                else if (trailing === 'call') onCall?.()
                else submit()
              }}
            >
              {(['stop.fill', 'waveform', 'arrow.up'] as const).map((name) => (
                <span key={name} aria-hidden="true" className={`composer-symbol ${name === symbol ? 'active' : ''}`}>
                  <Icon name={name} size={name === 'stop.fill' ? 12 : name === 'waveform' ? 15 : 14} weight="bold" />
                </span>
              ))}
            </button>
          </div>
        </div>
      </div>
      <Sheet open={voiceSettings} onClose={() => setVoiceSettings(false)}>
        <VoiceChatSettingsView />
      </Sheet>
    </VStack>
  )
}

/** Keep the outgoing row until its height/fade transition finishes. */
export function ComposerReveal({ open, children }: { open: boolean; children: ReactNode }) {
  const { mounted, shown } = usePresence(open)
  const last = useRef(children)
  if (open) last.current = children
  return <div className={`composer-reveal ${shown ? 'shown' : ''}`} inert={!open}>
    <div>{mounted ? (open ? children : last.current) : null}</div>
  </div>
}
