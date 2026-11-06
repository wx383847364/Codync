import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import type { Bot } from '@shared/models'
import { mentionQuery } from '@shared/mentions'
import { BotAvatar } from '../../components/Avatar'
import { usePresence } from '../../components/Overlay'

/** 管理光标处候选和键盘；IME 的确认键必须留给系统。 */
export function useMentionMenu(text: string, members: Bot[], select: (bot: Bot, start: number, end: number) => void) {
  const [selection, setSelection] = useState({ start: text.length, end: text.length })
  const [pick, setPick] = useState({ key: '', index: 0 })
  const [dismissed, setDismissed] = useState<string | null>(null)
  const query = mentionQuery(text, selection.start, selection.end)
  const key = JSON.stringify([text, selection.start, selection.end])
  // Esc 只关闭当前查询；编辑或移动光标后重新出现相同查询也应可以选择。
  useEffect(() => { if (dismissed !== key) setDismissed(null) }, [key, dismissed])
  // 搜索只负责呈现候选，成员资格仍由 host 校验。
  const shown = query && dismissed !== key ? members.filter((m) => m.name.toLocaleLowerCase().includes(query.query.toLocaleLowerCase())) : []
  const index = pick.key === key ? Math.min(pick.index, shown.length - 1) : 0
  /** 插入只替换查询片段，选择后隐藏菜单，后续编辑才重新打开。 */
  const choose = (bot: Bot) => {
    if (!query) return
    select(bot, query.start, query.end)
    setDismissed(key)
  }
  /** 拦截菜单按键，重复确认不会发送消息，Shift+Enter 仍由输入框换行。 */
  const onKey = (event: KeyboardEvent, composing: boolean) => {
    if (!shown.length || composing || event.altKey || event.metaKey || event.ctrlKey) return false
    const step = event.key === 'ArrowDown' ? 1 : event.key === 'ArrowUp' ? -1 : 0
    const confirm = !event.shiftKey && (event.key === 'Enter' || event.key === 'Tab')
    if (!step && !confirm && event.key !== 'Escape') return false
    event.preventDefault()
    event.stopPropagation()
    if (step) setPick({ key, index: (index + step + shown.length) % shown.length })
    else if (confirm && !event.repeat) choose(shown[index]!)
    else if (event.key === 'Escape') setDismissed(key)
    return true
  }
  // 鼠标与键盘共用同一高亮索引。
  return { shown, index, choose, onKey, hover: (index: number) => setPick({ key, index }), selection: setSelection }
}

/** 复用命令菜单的纵向布局及动画，同名候选始终显示 ID 辅助区分。 */
export function MentionMenu({ menu }: { menu: ReturnType<typeof useMentionMenu> }) {
  const open = menu.shown.length > 0
  const { mounted, shown } = usePresence(open)
  const last = useRef(menu.shown)
  if (open) last.current = menu.shown
  const rows = useRef<(HTMLButtonElement | null)[]>([])
  // 高亮移动时只滚动菜单，不转移输入焦点。
  useEffect(() => { void rows.current[menu.index]?.scrollIntoView({ block: 'nearest' }) }, [menu.index])
  if (!mounted) return null
  return <div className={`command-menu ${shown ? 'shown' : ''}`} inert={!open}>
    <div className="menu-panel" role="listbox" aria-label="Members">
      {(open ? menu.shown : last.current).map((bot, index) => (
        // 保留光标，悬停与键盘高亮同步。
        <button key={bot.id} ref={(element) => { rows.current[index] = element }} role="option" aria-selected={index === menu.index}
          className={`menu-row ${index === menu.index ? 'highlight' : ''}`} onMouseEnter={() => menu.hover(index)}
          onMouseDown={(event) => event.preventDefault()} onClick={() => menu.choose(bot)}>
          <BotAvatar bot={bot} size={18} animated={false} />
          <span>{bot.name}</span><span className="command-detail">{bot.id.slice(0, 8)}</span>
        </button>
      ))}
    </div>
  </div>
}
