import { useEffect, useState, type KeyboardEvent } from 'react'
import type { Bot } from '@shared/models'
import { mentionQuery, type Mention } from '@shared/mentions'

/** 管理光标处候选和键盘；IME 的确认键必须留给系统。 */
export function useMentionMenu(text: string, members: Bot[], select: (bot: Bot, start: number, end: number) => void, bindings: readonly Mention[] = []) {
  const [selection, setSelection] = useState({ start: text.length, end: text.length })
  const [pick, setPick] = useState({ key: '', index: 0 })
  const [dismissed, setDismissed] = useState<string | null>(null)
  const raw = mentionQuery(text, selection.start, selection.end)
  // 完整成员名后空格结束候选，仍允许在正文中间插入新成员。
  // 已选文件的 @ 标签不能再次解析为群成员候选。
  const query = raw && !bindings.some((binding) => binding.workspaceFile && binding.start <= raw.start && raw.start < binding.end) && !members.some((m) => raw.query.toLocaleLowerCase() === `${m.name.toLocaleLowerCase()} `) && !/^\s/.test(raw.query) ? raw : null
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
    const field = event.currentTarget as HTMLTextAreaElement
    if (event.key !== 'Escape' && (field.selectionStart !== selection.start || field.selectionEnd !== selection.end)) return false
    // 广播别名直接发送，Tab 仍可选择列表中的具名成员。
    if (event.key === 'Enter' && /^(all|everyone)\s*$/i.test(query?.query ?? '')) return false
    const step = event.key === 'ArrowDown' ? 1 : event.key === 'ArrowUp' ? -1 : 0
    const confirm = !event.shiftKey && (event.key === 'Enter' || event.key === 'Tab')
    if (!step && !confirm && event.key !== 'Escape') return false
    event.preventDefault()
    event.stopPropagation()
    // 连续方向键可能在同一轮渲染内到达，必须累加最新状态，首尾都按候选数回绕。
    if (step) setPick((previous) => {
      const current = previous.key === key ? Math.min(previous.index, shown.length - 1) : 0
      return { key, index: (current + step + shown.length) % shown.length }
    })
    else if (confirm && !event.repeat) choose(shown[index]!)
    else if (event.key === 'Escape') setDismissed(key)
    return true
  }
  // 鼠标与键盘共用同一高亮索引。
  return { shown, index, choose, onKey, hover: (index: number) => setPick({ key, index }), selection: setSelection }
}
