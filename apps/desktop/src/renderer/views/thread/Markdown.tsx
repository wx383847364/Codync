import { memo, type ReactNode } from 'react'
import { fileLinkPath } from '@shared/file-links'
import { FileLink } from './FileLink'
import { parseMarkdown, type Block } from './markdown-blocks.ts'
import { Hairline } from '../../components/Controls'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import './markdown.css'

// The Mac's Markdown for messages: the blocks from `markdown-blocks.ts`, with inline styling like
// `AttributedString(markdown:)`.

// 先匹配代码片段或完整链接，带反引号的文件名仍可成为链接标签。
const pattern = /(?<!`)(?<ticks>`+)(?!`)(?<code>[^\n]*?)(?<!`)\k<ticks>(?!`)|(?<!\\)(?<image>!?)\[(?<label>[^\[\]\n]+)\]\((?:<(?<wrapped>[^<>\n]+)>|(?<raw>(?:[^()\s]|\([^()\s]*\))+))\)|\*\*(?<bold>[^*]+)\*\*|__(?<strong>[^_]+)__|~~(?<strike>[^~]+)~~|\*(?<italic>[^*\n]+)\*|(?<![\w])_(?<em>[^_\n]+)_(?![\w])/

/** 渲染内联 Markdown，只有对话正文提供本地文件操作，代码块继续按文本渲染。 */
function inline(text: string, fileLinks: boolean, key = 'i'): ReactNode[] {
  const out: ReactNode[] = []
  let rest = text
  let n = 0
  while (rest) {
    const match = pattern.exec(rest)
    if (!match) {
      out.push(rest)
      break
    }
    if (match.index > 0) out.push(rest.slice(0, match.index))
    const k = `${key}${n++}`
    const { code, image, label, wrapped, raw, bold, strong, strike, italic, em } = match.groups!
    if (code !== undefined) {
      // 单独的绝对路径在历史回复中也可点击，保留行内代码的字样。
      const precedingBackslashes = /\\+$/.exec(rest.slice(0, match.index))?.[0].length ?? 0
      if (precedingBackslashes % 2 !== 0) {
        // 只消费被转义的一个反引号，后面的反引号仍可开启代码片段。
        out.push(match[0][0])
        rest = rest.slice(match.index + 1)
        continue
      }
      // CommonMark 去掉成对的一层边缘空格。
      const literal = code.startsWith(' ') && code.endsWith(' ') ? code.slice(1, -1) : code
      const path = fileLinks ? fileLinkPath(literal, true) : null
      const label = <code key={k} className="md-code-span">{code}</code>
      out.push(path ? <FileLink key={k} path={path}>{label}</FileLink> : label)
    } else if (label !== undefined) {
      const target = (wrapped ?? raw)!
      const children = inline(label, false, `${k}-`)
      const path = fileLinks ? fileLinkPath(target) : null
      // 图像标签只显示替代文字，只有明确的超链接能执行文件动作。
      if (image) out.push(...children)
      else if (path) out.push(<FileLink key={k} path={path}>{children}</FileLink>)
      else if (/^(https?|mailto|x-apple\.systempreferences):/i.test(target)) out.push(
        <a key={k} href={target} title={target} className="md-link" onClick={(event) => {
          // 普通网页链接仍交给系统浏览器处理。
          event.preventDefault()
          window.codync.app.openExternal(event.currentTarget.href)
        }}>{children}</a>,
      )
      else out.push(...children)
    } else if (bold !== undefined || strong !== undefined) out.push(<strong key={k} className="md-strong">{inline(bold ?? strong!, fileLinks, `${k}-`)}</strong>)
    else if (strike !== undefined) out.push(<s key={k}>{inline(strike, fileLinks, `${k}-`)}</s>)
    else out.push(<em key={k}>{inline(italic ?? em!, fileLinks, `${k}-`)}</em>)
    rest = rest.slice(match.index + match[0].length)
  }
  return out
}

/** 根据块类型渲染 Markdown，并将对话的文件操作范围传到每个内联链接。 */
const MarkdownBlock = memo(function MarkdownBlock({ block, fileLinks }: { block: Block; fileLinks: boolean }) {
  switch (block.kind) {
    case 'paragraph':
      return <div className="md-text md-spaced md-body">{inline(block.text, fileLinks)}</div>
    case 'heading':
      return (
        <div className={`md-text md-spaced md-heading-${Math.min(block.level, 3)}`}>
          {inline(block.text, fileLinks)}
        </div>
      )
    case 'bullet':
      return (
        <HStack align="baseline" gap={6} className="md-spaced md-body" style={{ paddingLeft: block.depth * 16 }}>
          <Text tone="secondary" className="md-marker">{block.marker}</Text>
          <span className="md-text md-bullet-text">{inline(block.text, fileLinks)}</span>
        </HStack>
      )
    case 'rule':
      return <Hairline className="md-rule" />
    case 'table': {
      const columns = Math.max(block.header.length, ...block.rows.map((r) => r.length))
      const cols = Array.from({ length: columns }, (_, i) => i)
      return (
        <div className="md-table-scroll">
          <div className="md-table md-body" style={{ gridTemplateColumns: `repeat(${columns}, max-content)` }}>
            {cols.map((i) => (
              <Text key={`h${i}`} weight="semibold">{inline(block.header[i] ?? '', fileLinks, `h${i}`)}</Text>
            ))}
            {block.rows.map((row, r) => [
              <Hairline key={`l${r}`} style={{ gridColumn: `1 / span ${columns}` }} />,
              ...cols.map((i) => <span key={`${r}-${i}`}>{inline(row[i] ?? '', fileLinks, `${r}-${i}`)}</span>),
            ])}
          </div>
        </div>
      )
    }
    case 'quote':
      return <div className="md-text md-spaced md-body md-quote">{inline(block.text, fileLinks)}</div>
    case 'code':
      return (
        <div className="md-code">
          <pre className="md-code-text">{block.text}</pre>
        </div>
      )
  }
}, (a, b) => {
  // 文件操作权限和文本都相同，才可复用块。
  return a.fileLinks === b.fileLinks && JSON.stringify(a.block) === JSON.stringify(b.block)
})

/** 渲染消息 Markdown，复用未变化的块。 */
export const MarkdownText = memo(function MarkdownText({ text, fileLinks = false }: { text: string; fileLinks?: boolean }) {
  const blocks = parseMarkdown(text)
  return (
    <VStack gap={10} shrink className="markdown selectable">
      {/* 表格、列表等块继承消息的链接作用域。 */}
      {blocks.map((b, i) => (
        <MarkdownBlock key={i} block={b} fileLinks={fileLinks} />
      ))}
    </VStack>
  )
})
