export type FileLinkEditor = 'cursor' | 'vscode'
export type FileLinkAction = 'open' | 'reveal' | 'save' | 'copy_contents' | FileLinkEditor
export type FileLinkResult = 'ok' | 'cancelled' | 'missing' | 'unsupported' | 'failed'

/** 将 Markdown 目标还原为绝对路径。行内代码中的百分号按原样保留。 */
export function fileLinkPath(destination: string, literal = false): string | null {
  const fileURL = !literal && /^file:\/\/(?:localhost)?(?=\/)/i.exec(destination)
  const encoded = fileURL ? destination.slice(fileURL[0].length) : destination
  let path: string
  try { path = literal ? encoded : decodeURIComponent(encoded) } catch {
    if (fileURL) return null
    path = encoded
  }
  if (fileURL && /^\/[a-z]:[\\/]/i.test(path)) path = path.slice(1)
  if (path.length > 32768 || /[\x00-\x1f\x7f]/.test(path)) return null
  if (/^\/(?![\\/])/.test(path) || /^[a-z]:[\\/]/i.test(path)) return path
  return null
}

/** 识别浏览器可直接打开的本地 HTML 扩展名，路径已由 Markdown 解码。 */
export function isHTMLFileLink(path: string): boolean {
  return /\.html?$/i.test(path)
}
