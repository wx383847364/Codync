import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import type { Bot } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { Icon } from '../../components/Icon'
import { usePresence } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import type { SlashCommand } from './slash-commands'

interface ComposerMenuState<T> {
  shown: T[]
  status?: string
  index: number
  hover: (index: number) => void
  pick: (item: T) => void
}

/**
 * The highlight and keys of a list above the box (`/` commands, `@` members): ↑/↓ move, Tab or Return picks,
 * Escape hides the list until the draft changes. `returnPicks` lets Return fall through to sending for a row.
 * Both lists are for the query at the end of the draft, so with the caret elsewhere or text selected
 * every key but Escape edits the draft and Return sends.
 */
/** 统一候选按键；方向键逐次循环，光标离开末尾查询后交回编辑器。 */
export function useComposerMenu<T>(draft: string, items: T[], pick: (item: T) => void, returnPicks: (item: T, draft: string) => boolean = () => true, options: { caret?: number; status?: string; loading?: boolean } = {}) {
  const { caret = draft.length, status = '', loading = false } = options
  const [highlight, setHighlight] = useState({ draft: '', index: 0 })
  const [dismissed, setDismissed] = useState<string | null>(null)
  if (dismissed !== null && dismissed !== draft) setDismissed(null)
  const shown = dismissed === draft ? [] : items
  const index = highlight.draft === draft ? Math.min(highlight.index, shown.length - 1) : 0

  /** Handles a key in the field while the list is up; returns whether it did. */
  // IME 与组合快捷键交给输入框，确认只消费当前候选。
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>, composing: boolean): boolean => {
    const picked = shown[index]
    if (composing || e.altKey || e.metaKey || e.ctrlKey || dismissed === draft) return false
    const { selectionStart, selectionEnd } = e.currentTarget
    if ((selectionStart !== caret || selectionEnd !== caret) && e.key !== 'Escape') return false
    if (!picked) {
      // 等待匹配时按回车不能误发；Esc 可随时返回普通输入。
      if (e.key === 'Escape' && status) setDismissed(draft)
      else if (!loading || e.key !== 'Enter' || e.shiftKey) return false
      e.preventDefault()
      return true
    }
    const step = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0
    const picks = (e.key === 'Tab' && !e.shiftKey) || (e.key === 'Enter' && !e.shiftKey && returnPicks(picked, draft))
    if (!step && !picks && e.key !== 'Escape') return false
    e.preventDefault()
    e.stopPropagation()
    // 同帧方向键累加最新高亮，重复确认只消费按键而不再次执行。
    if (step) setHighlight((previous) => ({ draft, index: ((previous.draft === draft ? Math.min(previous.index, shown.length - 1) : 0) + step + shown.length) % shown.length }))
    else if (picks) { if (!e.repeat) pick(picked) }
    else setDismissed(draft)
    return true
  }

  return { shown, index, status: dismissed === draft ? '' : status, hover: (i: number) => setHighlight({ draft, index: i }), pick, onKey }
}

/** A list over the transcript, above the box: each row's icon, name and what it is. */
function ComposerMenu<T>({ menu, label, row }: {
  menu: ComposerMenuState<T>
  label: string
  row: (item: T) => { key: string; icon: ReactNode; title: string; detail: string; warning?: boolean }
}) {
  const open = menu.shown.length > 0 || !!menu.status
  const { mounted, shown } = usePresence(open)
  const last = useRef(menu.shown)
  if (open) last.current = menu.shown
  const list = open ? menu.shown : last.current
  const rows = useRef<(HTMLButtonElement | null)[]>([])
  // Braces: newer Chromium returns a promise from `scrollIntoView`, which an effect mustn't return.
  useEffect(() => {
    void rows.current[menu.index]?.scrollIntoView({ block: 'nearest' })
  }, [menu.index, list])
  if (!mounted) return null
  return (
    <div className={`composer-menu ${shown ? 'shown' : ''}`} inert={!open}>
      <div className="menu-panel" role="listbox" aria-label={label}>
        {menu.status && <div className="menu-row" role="status"><Text tone="secondary">{menu.status}</Text></div>}
        {/* 候选点击和键盘共用同一个插入或执行操作。 */}
        {list.map((item, i) => {
          const { key, icon, title, detail, warning } = row(item)
          return (
            <button
              key={key}
              ref={(el) => { rows.current[i] = el }}
              role="option"
              aria-selected={i === menu.index}
              className={`menu-row ${i === menu.index ? 'highlight' : ''}`}
              title={detail || title}
              onMouseEnter={() => menu.hover(i)}
              // Keep the caret in the field.
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => menu.pick(item)}
            >
              <span className="menu-icon">{icon}</span>
              <Text truncate>{title}</Text>
              <Text truncate grow tone={warning ? 'attention' : 'secondary'}>{detail}</Text>
            </button>
          )
        })}
      </div>
    </div>
  )
}

/** The `/` list: each command's icon, name and what it does. */
export function CommandMenu({ menu }: { menu: ComposerMenuState<SlashCommand> }) {
  const tr = useTr()
  return <ComposerMenu menu={menu} label={tr('Commands')} row={(c) => ({
    key: `${c.action.kind}:${c.name}`,
    icon: <Icon name={c.icon} size={12} color="var(--secondary)" />,
    title: c.action.kind === 'file' ? c.title : tr(c.title),
    detail: c.action.kind === 'file' ? c.detail : tr(c.detail),
    warning: c.warning,
  })} />
}

/** The `@` list in a group: each member's face, name and description. */
/** 成员列表复用命令布局，并显示稳定 ID 以区分同名成员。 */
export function MentionMenu({ menu }: { menu: ComposerMenuState<Bot> }) {
  const tr = useTr()
  return <ComposerMenu menu={menu} label={tr('Members')} row={(m) => ({
    key: m.id,
    icon: <BotAvatar bot={m} size={18} animated={false} />,
    title: m.name,
    detail: `${m.id.slice(0, 8)}${m.description ? ` · ${m.description}` : ''}`,
  })} />
}
