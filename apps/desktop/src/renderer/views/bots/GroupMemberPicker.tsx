import { useEffect, useId, useRef, useState, type RefObject } from 'react'
import type { Bot } from '@shared/models'
import { BotAvatar } from '../../components/Avatar'
import { Icon } from '../../components/Icon'
import { useEscape } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'

/** 管理同宽内联列表的展开状态、键盘返回和外部点击。 */
function useBotOptions(trigger: RefObject<HTMLElement | null>) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const listId = useId()

  /** 收起后把键盘焦点留在原控件。 */
  function close() {
    setOpen(false)
    trigger.current?.focus()
  }

  useEscape(open, close, root)
  useEffect(() => {
    if (!open) return
    /** 外部点击完成后再收起，避免内联列表改变表单位置而吞掉点击。 */
    const dismissOnOutsideClick = (event: MouseEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false)
    }
    document.addEventListener('click', dismissOnOutsideClick)
    return () => document.removeEventListener('click', dismissOnOutsideClick)
  }, [open])

  /** 方向键先展开，再把焦点移至首个可选项。 */
  function focusOptions() {
    if (open) root.current?.querySelector<HTMLElement>('[role="option"]')?.focus()
    else setOpen(true)
  }

  return { open, setOpen, root, listId, close, focusOptions }
}

/** 三个下拉共用同一组头像行，只有添加成员时显示加号。 */
function BotOptions({ id, label, bots, selected, onPick, trigger, multiple = false, allowsNone = false }: {
  id: string
  label: string
  bots: Bot[]
  selected: ReadonlySet<string>
  onPick: (bot?: Bot) => void
  trigger: RefObject<HTMLElement | null>
  multiple?: boolean
  allowsNone?: boolean
}) {
  const tr = useTr()
  return (
    <div id={id} className="menu-panel group-bot-options" role="listbox" aria-multiselectable={multiple || undefined}
      aria-label={label} onKeyDown={(event) => {
        // 候选行用方向键浏览，首行向上回到触发控件。
        if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
        if (!(event.target instanceof HTMLButtonElement)) return
        event.preventDefault()
        const next = event.key === 'ArrowDown' ? event.target.nextElementSibling : event.target.previousElementSibling ?? trigger.current
        if (next instanceof HTMLElement) next.focus()
      }}>
      {allowsNone && <button type="button" className="menu-row group-bot-option" role="option"
        aria-selected={!selected.size} onClick={() => onPick()}>
        <span className="group-bot-option-spacer" aria-hidden />
        <Text truncate>{tr('No lead')}</Text>
        {!selected.size && <Icon name="checkmark" size={13} className="group-bot-option-mark" />}
      </button>}
      {bots.length === 0 && !allowsNone && <Text as="div" className="group-member-empty" tone="secondary">{tr('No matching bots')}</Text>}
      {bots.map((bot) => {
        const on = selected.has(bot.id)
        return <button key={bot.id} type="button" className="menu-row group-bot-option" role="option"
          aria-selected={on} onClick={() => onPick(bot)} title={bot.name}>
          <BotAvatar bot={bot} size={22} animated={false} />
          <Text truncate>{bot.name}</Text>
          {(on || multiple) && <Icon name={on ? 'checkmark' : 'plus'} size={13} className="group-bot-option-mark" />}
        </button>
      })}
    </div>
  )
}

/** 从本机候选名单添加成员，头像网格只显示已加入的机器人。 */
export function GroupMemberPicker({ bots, picked, onToggle }: {
  bots: Bot[]
  picked: Bot[]
  onToggle: (bot: Bot) => void
}) {
  const tr = useTr()
  const [query, setQuery] = useState('')
  const input = useRef<HTMLInputElement>(null)
  const { open, setOpen, root, listId, focusOptions } = useBotOptions(input)
  const chosen = new Set(picked.map((bot) => bot.id))
  const needle = query.trim().toLocaleLowerCase()
  const results = bots.filter((bot) => bot.name.toLocaleLowerCase().includes(needle))

  return (
    <>
      <div className="group-member-picker" ref={root}>
        <div className="field-box group-member-search">
          <Icon name="magnifyingglass" size={15} />
          <input
            ref={input}
            type="search"
            placeholder={tr('Search or add bots')}
            aria-label={tr('Search or add bots')}
            aria-expanded={open}
            aria-controls={open ? listId : undefined}
            role="combobox"
            value={query}
            onChange={(event) => {
              // 输入、清空搜索词时即时刷新名单。
              setQuery(event.target.value)
              setOpen(true)
            }}
            onKeyDown={(event) => {
              // 向下进入已展开的名单，按回车不会提交建群表单。
              if (event.key === 'ArrowDown') {
                event.preventDefault()
                focusOptions()
              }
            }}
          />
          <button type="button" aria-label={tr('Show bot list')} aria-expanded={open} onClick={() => {
            // 展开箭头始终提供完整名单。
            if (!open) setQuery('')
            setOpen(!open)
          }}>
            <Icon name={open ? 'chevron.up' : 'chevron.down'} size={12} />
          </button>
        </div>
        {open && <BotOptions id={listId} label={tr('Bots')} bots={results} selected={chosen}
          trigger={input} multiple onPick={(bot) => { if (bot) onToggle(bot) }} />}
      </div>
      {picked.length > 0 && <div className="create-members">
        {picked.map((bot) => (
          <button key={bot.id} type="button" className="press create-member"
            aria-label={tr('Remove {name}').replace('{name}', () => bot.name)} title={bot.name} onClick={() => onToggle(bot)}>
            <span className="swatch on"><BotAvatar bot={bot} size={44} animated={false} /></span>
            <Text size="caption" truncate className="create-member-name">{bot.name}</Text>
          </button>
        ))}
      </div>}
    </>
  )
}

/** 负责人和角色编辑使用同一个单选控件，候选限定为已经加入的机器人。 */
export function GroupBotPicker({ bots, selected, onChange, label, allowsNone = false, showsLabel = true }: {
  bots: Bot[]
  selected?: string
  onChange: (id?: string) => void
  label: string
  allowsNone?: boolean
  showsLabel?: boolean
}) {
  const tr = useTr()
  const trigger = useRef<HTMLButtonElement>(null)
  const { open, setOpen, root, listId, close, focusOptions } = useBotOptions(trigger)
  const current = bots.find((bot) => bot.id === selected)
  const title = tr(label)
  const valueId = useId()
  return (
    <div className="group-member-picker" ref={root}>
      <button type="button" ref={trigger} className="field-box group-bot-choice" disabled={!bots.length}
        aria-label={title} aria-describedby={valueId} aria-haspopup="listbox" aria-expanded={open} aria-controls={open ? listId : undefined}
        onClick={() => setOpen(!open)} onKeyDown={(event) => {
          // 允许键盘直接展开或进入名单。
          if (event.key !== 'ArrowDown') return
          event.preventDefault()
          focusOptions()
        }}>
        {showsLabel && <span className="group-bot-choice-label">{title}</span>}
        <Text id={valueId} tone={showsLabel ? 'secondary' : 'text'} truncate>{current?.name ?? tr(allowsNone ? 'No lead' : 'Choose')}</Text>
        <Icon name={open ? 'chevron.up' : 'chevron.down'} size={12} className="group-bot-option-mark" />
      </button>
      {open && <BotOptions id={listId} label={title} bots={bots} selected={new Set(current ? [current.id] : [])}
        trigger={trigger} allowsNone={allowsNone} onPick={(bot) => {
          // 单选提交后收起名单，再继续原表单。
          onChange(bot?.id)
          close()
        }} />}
    </div>
  )
}
