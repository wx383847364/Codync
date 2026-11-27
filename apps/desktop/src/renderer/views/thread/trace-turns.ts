import type { Entry } from '../../../shared/models.ts'

/** 按回合分组并保留组内顺序，逐条追加以避免长回合反复复制已有记录。 */
export function traceTurns(entries: Entry[]): [number, Entry[]][] {
  const turns = new Map<number, Entry[]>()
  for (const entry of entries) {
    const turn = turns.get(entry.turn)
    if (turn) turn.push(entry)
    else turns.set(entry.turn, [entry])
  }
  return [...turns.entries()].sort((a, b) => a[0] - b[0])
}

/** The turn running now while the bot works in this chat; a queued message already holds the next turn. */
export function runningTurn(entries: Entry[], working: boolean): number | undefined {
  if (!working) return undefined
  return entries.reduce((t, e) => (e.data.status === 'queued' ? t : Math.max(t, e.turn)), -1)
}

/** What the running turn has done so far, in order: the steps the working indicator unfolds. */
export function workingSteps(entries: Entry[]): Entry[] {
  const turn = runningTurn(entries, true)
  return entries.filter((e) => e.turn === turn && isWorkingStep(e))
}

/** Thinking, narration, a tool call or a plan; never the bot's bookkeeping, nor a call the host didn't flag. */
function isWorkingStep(e: Entry): boolean {
  switch (e.kind) {
    case 'thought':
      return !!e.data.text?.trim()
    case 'agent':
      return !e.data.final && !isPass(e.data.text)
    case 'tool':
      return e.data.bookkeeping === false
    case 'plan':
      return true
    default:
      return false
  }
}

/** No text, or a pass the host keeps out of the chat (`is_pass` in host/src/chat/group.rs). */
function isPass(text = ''): boolean {
  const t = text.trim().replace(/^[".*]+|[".*]+$/g, '').toLowerCase()
  return t === '' || t === '(pass)' || t === 'pass'
}

/** Scrolled to (within a few pixels of) the end: new steps keep it there. */
export function scrolledToEnd(el: { scrollHeight: number; scrollTop: number; clientHeight: number }) {
  return el.scrollHeight - el.scrollTop - el.clientHeight < 8
}
