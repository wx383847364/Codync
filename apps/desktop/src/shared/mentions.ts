export interface WorkspaceFileReference { path: string; workspace: string }
export interface WorkspaceFileCandidate { name: string; source: string; file: WorkspaceFileReference }

/** 固定成员或工作区文件身份，偏移使用请求正文的 UTF-16 半开区间。 */
export interface Mention { botId?: string; start: number; end: number; workspaceFile?: WorkspaceFileReference }

/** 根据一次文本编辑平移未受影响的绑定；编辑触及片段时解除身份绑定。 */
export function editMentions(before: string, after: string, mentions: readonly Mention[], replacement?: { start: number; end: number }): Mention[] {
  // 原生删除键常只报告折叠光标；真实删除范围交给保守差异算法，避免保留已删片段。
  if (replacement && !(replacement.start === replacement.end && after.length < before.length)) {
    const delta = after.length - before.length
    // 原生替换只移动完全在外的身份，触及片段或扩展名字则解除。
    return mentions.flatMap((m) => {
      if (m.end <= replacement.start && !(m.end === replacement.start && delta > 0 && /[\p{L}\p{N}_]/u.test(String.fromCodePoint(after.codePointAt(m.end) ?? 0)))) return [{ ...m }]
      if (m.start >= replacement.end) return [{ ...m, start: m.start + delta, end: m.end + delta }]
      return []
    })
  }
  if (before === after) return [...mentions]
  let start = 0
  while (start < before.length && start < after.length && before[start] === after[start]) start++
  let suffix = 0
  while (suffix < before.length && suffix < after.length && before[before.length - suffix - 1] === after[after.length - suffix - 1]) suffix++
  const removed = Math.max(0, before.length - after.length, before.length - start - suffix)
  const earliest = before.length - suffix - removed
  const latestEnd = start + removed
  const delta = after.length - before.length
  return mentions.flatMap((m) => {
    // 在片段边界插入属于外部编辑，片段内部插入则解除绑定。
    if (m.end <= earliest && !(m.end === earliest && delta > 0 && /[\p{L}\p{N}_]/u.test(String.fromCodePoint(after.codePointAt(m.end) ?? 0)))) return [{ ...m }]
    if (m.start >= latestEnd) return [{ ...m, start: m.start + delta, end: m.end + delta }]
    return []
  })
}

/** 仅定位光标前的 @ 查询，不会吞掉光标后的正文或非空选区。 */
export function mentionQuery(text: string, start: number, end = start): { start: number; end: number; query: string } | null {
  if (start !== end) return null
  const at = text.lastIndexOf('@', start - 1)
  if (at < 0 || (at > 0 && !/\s/u.test(text[at - 1]!))) return null
  const query = text.slice(at + 1, start)
  return /[\r\n]/u.test(query) || query.length > 24 ? null : { start: at, end: start, query }
}
