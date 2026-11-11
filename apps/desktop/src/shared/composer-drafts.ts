import { editMentions, type Mention } from './mentions.ts'

/** Drafts saved before 2.20.1 were keyed `["<bot>",null]`; a thread's (`["<bot>","<root>"]`) is dropped. */
function chatOf(key: string): string | null {
  let old: unknown
  try { old = JSON.parse(key) } catch { return key }
  if (!Array.isArray(old)) return key
  return old.length === 2 && typeof old[0] === 'string' && old[1] === null ? old[0] : null
}

/** Text belongs to a chat, independent of a mounted editor or transcript cache. */
export class ComposerDrafts {
  private values = new Map<string, string>()
  private bindings = new Map<string, Mention[]>()
  private listeners = new Set<() => void>()
  private retired = false

  private storage: Pick<Storage, 'getItem' | 'setItem'>
  private key: string
  private onSaveError?: () => void

  /** 读取兼容的文本草稿，并恢复可用的身份绑定元数据。 */
  constructor(storage: Pick<Storage, 'getItem' | 'setItem'>, key: string, onSaveError?: () => void) {
    this.storage = storage
    this.key = key
    this.onSaveError = onSaveError
    try {
      const saved: unknown = JSON.parse(storage.getItem(key) ?? 'null')
      if (Array.isArray(saved)) {
        for (const row of saved) {
          if (Array.isArray(row) && row.length >= 2 && typeof row[0] === 'string' && typeof row[1] === 'string') {
            const chat = chatOf(row[0])
            if (chat) this.values.set(chat, row[1])
            if (Array.isArray(row[2])) this.bindings.set(chat!, row[2])
          }
        }
      }
    } catch { /* A damaged or unavailable store must not prevent typing. */ }
  }

  subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  get(bot: string) {
    return this.values.get(bot) ?? ''
  }

  /** 读取与草稿一起持久化的身份，不按当前成员名重建。 */
  mentions(bot: string): Mention[] { return this.bindings.get(bot) ?? [] }

  /** 选择时先根据正文移动旧绑定，再保存新增绑定。 */
  selectMention(bot: string, text: string, mention: Mention, replacement?: { start: number; end: number }) {
    this.set(bot, text, replacement, mention)
  }

  /** 正文和更新后的引用一起写入，选中引用后也只保存一次。 */
  set(bot: string, text: string, replacement?: { start: number; end: number }, selected?: Mention) {
    if (this.retired) return
    const bindings = editMentions(this.get(bot), text, this.mentions(bot), replacement)
    if (selected) bindings.push(selected)
    this.bindings.set(bot, bindings)
    if (text) this.values.set(bot, text)
    else this.values.delete(bot)
    this.save()
  }

  /** The chat's draft and its artifacts' (`artifactDraftKey`). */
  /** 移除目标及附件草稿时同步清理身份范围。 */
  removeBot(bot: string) {
    if (this.retired) return
    for (const key of [...this.values.keys()]) if (key === bot || key.startsWith(`${bot}/`)) { this.values.delete(key); this.bindings.delete(key) }
    this.save()
  }

  /** 清空全部草稿与身份，避免跨账户残留。 */
  clear() {
    if (this.retired) return
    this.values.clear()
    this.bindings.clear()
    this.save()
  }

  retire() {
    this.retired = true
  }

  /** 把正文与绑定一起持久化，刷新或离线后身份不变。 */
  private save() {
    // Save on each edit, including clearing after submission; no unmount/debounce race.
    try { this.storage.setItem(this.key, JSON.stringify([...this.values].map(([bot, text]) => this.mentions(bot).length ? [bot, text, this.mentions(bot)] : [bot, text]))) } catch { this.onSaveError?.() }
    for (const listener of this.listeners) listener()
  }
}
