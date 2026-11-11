export const FILE_TEXT_LIMIT = 1024 * 1024
export const FILE_TEXT_LIMIT_MESSAGE = 'Copy file contents supports UTF-8 text files up to 1 MiB.'

export type FileLinkEditor = 'cursor' | 'vscode'
export type FileLinkAction = 'open' | 'reveal' | 'save' | 'copy_contents' | FileLinkEditor
export type SharedFileAction = FileLinkAction | 'copy_path'
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


/** Files delivered by a bot may come from another computer. Open documents through their
 * normal associations and leave executables and shell shortcuts available for Save as. */
export function canOpenSharedFile(name: string, platform: string): boolean {
  const extension = name.split('.').pop()?.toLowerCase() ?? ''
  if (['app', 'desktop', 'inetloc', 'lnk', 'url', 'webloc'].includes(extension)) return false
  return platform !== 'win32' || !/^(?:appref-ms|application|bat|cmd|com|cpl|exe|hta|inf|jar|js|jse|msc|msi|msp|pif|ps1|reg|scf|scr|sct|vb|vbe|vbs|wsf|wsh)$/.test(extension)
}
