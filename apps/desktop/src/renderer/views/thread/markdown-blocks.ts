// Block-level Markdown as replies use it (kit MarkdownBlocks): paragraphs, headings, nested
// lists, quotes, rules, tables and fenced code. Pure parsing, tested on its own.

export type Block =
  | { kind: 'paragraph'; text: string }
  | { kind: 'heading'; text: string; level: number }
  | { kind: 'bullet'; text: string; marker: string; depth: number }
  | { kind: 'quote'; text: string }
  | { kind: 'code'; text: string; language: string }
  | { kind: 'table'; header: string[]; rows: string[][] }
  | { kind: 'rule' }

const isTableRule = (line: string) => line.includes('-') && [...line].every((c) => '|-: '.includes(c))
const cells = (line: string) => {
  let row = line
  if (row.startsWith('|')) row = row.slice(1)
  if (row.endsWith('|')) row = row.slice(0, -1)
  return row.split('|').map((c) => c.trim())
}

/** 划分 Markdown 块。代码围栏内的文件链接示例保持为代码。 */
export function parseMarkdown(source: string): Block[] {
  const blocks: Block[] = []
  let paragraph: string[] = []
  let table: string[] = []
  let code: string[] | null = null
  let language = ''
  let fence = ''
  // 按已有的段落及表格规则保存累计内容。
  const flush = () => {
    if (paragraph.length) {
      blocks.push({ kind: 'paragraph', text: paragraph.join('\n') })
      paragraph = []
    }
    if (table.length) {
      // A header and its `|---|` line make a table; anything less stays text.
      if (table.length >= 2 && isTableRule(table[1]!)) blocks.push({ kind: 'table', header: cells(table[0]!), rows: table.slice(2).map(cells) })
      else blocks.push({ kind: 'paragraph', text: table.join('\n') })
      table = []
    }
  }
  for (const raw of source.split('\n')) {
    const line = raw.trim()
    const marker = /^(?:`{3,}|~{3,})/.exec(line)?.[0]
    if (code) {
      // 闭合围栏须使用相同标记，且长度至少为起始围栏的长度。
      if (marker?.startsWith(fence) && marker.length === line.length) {
        blocks.push({ kind: 'code', text: code.join('\n'), language })
        code = null
      } else code.push(raw)
      continue
    }
    if (marker) {
      flush()
      code = []
      fence = marker
      language = line.slice(marker.length)
      continue
    }
    if (line.startsWith('|')) {
      if (paragraph.length) flush()
      table.push(line)
      continue
    }
    if (table.length) flush()
    const depth = Math.min(Math.floor((/^[ \t]*/.exec(raw)?.[0].length ?? 0) / 2), 3)
    const heading = /^(#+) (.*)$/.exec(line)
    const ordered = /^(\d+\.) (.*)$/.exec(line)
    if (!line) flush()
    else if (line === '---' || line === '***' || line === '___') {
      flush()
      blocks.push({ kind: 'rule' })
    } else if (heading) {
      flush()
      blocks.push({ kind: 'heading', text: heading[2]!.trim(), level: heading[1]!.length })
    } else if (/^[-*+•] /.test(line)) {
      flush()
      blocks.push({ kind: 'bullet', text: line.slice(2), marker: depth > 0 ? '◦' : '•', depth })
    } else if (ordered) {
      flush()
      blocks.push({ kind: 'bullet', text: ordered[2]!, marker: ordered[1]!, depth })
    } else if (line.startsWith('>')) {
      flush()
      blocks.push({ kind: 'quote', text: line.slice(1).trim() })
    } else paragraph.push(raw)
  }
  if (code) blocks.push({ kind: 'code', text: (code as string[]).join('\n'), language })
  flush()
  return blocks
}
