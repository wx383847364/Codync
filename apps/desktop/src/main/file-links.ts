import { execFile, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { constants, createWriteStream } from 'node:fs'
import { open, realpath, rename, rm, stat } from 'node:fs/promises'
import { basename, dirname, isAbsolute, join } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'
import { app, clipboard, dialog, ipcMain, shell, type BrowserWindow } from 'electron'
import { isHTMLFileLink, type FileLinkAction, type FileLinkEditor, type FileLinkResult } from '../shared/file-links'

const actions = new Set<FileLinkAction>(['open', 'reveal', 'save', 'copy_contents', 'cursor', 'vscode'])
const editors: Record<FileLinkEditor, string> = { cursor: 'Cursor.app', vscode: 'Visual Studio Code.app' }
const run = promisify(execFile)
const maxCopy = 1024 * 1024
const readFlags = constants.O_RDONLY | (process.platform === 'win32' ? 0 : constants.O_NONBLOCK | constants.O_NOFOLLOW)

/** 只有当前系统的本地绝对路径能传给系统文件 API。Windows 设备名、备用数据流和 UNC 都不在范围内。 */
function isNativePath(path: string) {
  if (path.length > 32768 || /[\x00-\x1f\x7f]/.test(path)) return false
  if (process.platform !== 'win32') return /^\/(?![\\/])/.test(path)
  return /^[a-z]:[\\/]/i.test(path) && !path.slice(2).split(/[\\/]/).some((part) =>
    /[:*?"<>|]/.test(part) || /^(?:con|prn|aux|nul|com[0-9¹²³]|lpt[0-9¹²³])(?:\s*\.|\s*$)/i.test(part))
}

/** 查找 macOS 上已安装的编辑器，编辑器名称由应用固定。 */
async function editorApp(editor: FileLinkEditor) {
  if (process.platform !== 'darwin') return null
  for (const folder of ['/Applications', join(app.getPath('home'), 'Applications')]) {
    const bundle = join(folder, editors[editor])
    const info = await stat(join(bundle, 'Contents', 'Info.plist')).catch(() => null)
    if (info?.isFile()) return bundle
  }
  return null
}

/** 将文件保存到用户选择的位置，成功前保留同名旧文件。 */
async function saveAs(window: BrowserWindow, source: string, name: string): Promise<FileLinkResult> {
  const { canceled, filePath } = await dialog.showSaveDialog(window, { defaultPath: name })
  if (canceled || !filePath) return 'cancelled'
  if (filePath === source) return 'ok'
  const input = await open(source, readFlags)
  const staging = join(dirname(filePath), `.codync-${randomUUID()}.partial`)
  // 先写入目标目录，再在同一文件系统内替换，失败时清除本次写入。
  try {
    if (!(await input.stat()).isFile()) return 'unsupported'
    await pipeline(input.createReadStream({ autoClose: false }), createWriteStream(staging, { flags: 'wx', mode: 0o600 }))
    await rename(staging, filePath)
  } finally {
    try { await input.close() } finally { await rm(staging, { force: true }) }
  }
  return 'ok'
}

/** 只将大小受限的 UTF-8 文本放入剪贴板。读取时重新检查文件类型，避免追随换成管道的路径。 */
async function copyContents(path: string): Promise<FileLinkResult> {
  const file = await open(path, readFlags)
  try {
    const info = await file.stat()
    if (!info.isFile() || info.size > maxCopy) return 'unsupported'
    const bytes = Buffer.alloc(info.size + 1)
    let length = 0
    while (length < bytes.length) {
      const { bytesRead } = await file.read(bytes, length, bytes.length - length)
      if (!bytesRead) break
      length += bytesRead
    }
    if (length !== info.size) return 'failed'
    let content: string
    try { content = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)) }
    catch { return 'unsupported' }
    if (/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(content)) return 'unsupported'
    clipboard.writeText(content)
    return 'ok'
  } finally {
    await file.close()
  }
}

/** 仅把不存在的路径判作缺失，权限错误等交给调用者提示。 */
async function resolveIfPresent(path: string) {
  if (!isNativePath(path)) return null
  try { return await realpath(path) } catch (error) {
    const code = (error as NodeJS.ErrnoException).code
    if (code === 'ENOENT' || code === 'ENOTDIR') return null
    throw error
  }
}

/** 按 XDG 桌面文件 ID 查找启动项。ID 中的连字符也可以代表目录分隔符。 */
async function desktopEntry(folder: string, id: string): Promise<string | null> {
  const entry = join(folder, id)
  if ((await stat(entry).catch(() => null))?.isFile()) return entry
  for (let index = id.indexOf('-'); index !== -1; index = id.indexOf('-', index + 1)) {
    const part = id.slice(0, index)
    if (!part || part === '.' || part === '..') continue
    const subfolder = join(folder, part)
    // 只进入与 ID 前缀匹配的目录，不遍历整个应用列表。
    if (!(await stat(subfolder).catch(() => null))?.isDirectory()) continue
    const nested = await desktopEntry(subfolder, id.slice(index + 1))
    if (nested) return nested
  }
  return null
}

/** 用 Linux 的桌面条目启动浏览器，保留 Flatpak 等安装方式的参数。 */
async function openLinuxHTML(path: string) {
  const { stdout } = await run('xdg-mime', ['query', 'default', 'x-scheme-handler/https'], { timeout: 5000 })
  const desktop = stdout.trim()
  if (!/^[a-z0-9_.+-]+\.desktop$/i.test(desktop)) throw new Error('No default browser')
  const home = process.env.XDG_DATA_HOME || join(app.getPath('home'), '.local/share')
  const dirs = (process.env.XDG_DATA_DIRS || '/usr/local/share:/usr/share').split(':')
  for (const dir of [home, ...dirs].filter(isAbsolute)) {
    const entry = await desktopEntry(join(dir, 'applications'), desktop)
    if (entry) {
      // 等待 GIO 自己退出，浏览器可以继续运行且不会持有 Node 的输出管道。
      const child = spawn('gio', ['launch', entry, pathToFileURL(path).href], { stdio: 'ignore', timeout: 5000 })
      const [code] = await once(child, 'close')
      if (code !== 0) throw new Error('Could not launch the default browser')
      return
    }
  }
  throw new Error('Default browser entry not found')
}

/** 按系统的 HTTPS 浏览器关联打开原 HTML，保留同目录资源的相对路径。 */
async function openHTML(path: string): Promise<void> {
  if (process.platform === 'linux') {
    try { return await openLinuxHTML(path) } catch (error) {
      // 最小化系统可能没有 XDG/GIO 命令，由 Electron 查询同一 HTTPS 关联。
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
    }
  }
  // 查询网页协议的默认应用，避免 HTML 扩展名被编辑器接管。
  const browser = (await app.getApplicationInfoForProtocol('https://example.com')).path
  if (!isNativePath(browser)) throw new Error('Invalid default browser path')
  if (process.platform === 'darwin') {
    await run('/usr/bin/open', ['-a', browser, path], { timeout: 5000 })
    return
  }
  // 直接启动的浏览器会继续运行，进程启动后即可释放引用。
  const child = spawn(browser, [pathToFileURL(path).href], { detached: true, stdio: 'ignore' })
  await once(child, 'spawn')
  child.unref()
}

/** 对明确选择的链接执行系统操作，回传固定类别供界面提示及匿名统计。 */
async function perform(window: BrowserWindow, path: string, action: FileLinkAction): Promise<FileLinkResult> {
  // 带行号的目标是源码引用，去掉行号后仍保留原来的默认应用操作。
  const opensInBrowser = isHTMLFileLink(path)
  // 优先使用原路径，只有文件不存在时才尝试去掉 Codex 的 :行号[:列号]。
  let resolved = await resolveIfPresent(path)
  if (!resolved) {
    const file = path.replace(/:\d+(?::\d+)?$/, '')
    if (!isNativePath(file)) return 'unsupported'
    if (file !== path) { path = file; resolved = await resolveIfPresent(path) }
  }
  if (!resolved) return 'missing'
  if (!isNativePath(resolved)) return 'unsupported'
  const info = await stat(resolved)
  if (!info.isFile() && !info.isDirectory()) return 'unsupported'
  switch (action) {
    case 'open':
      if (info.isFile() && opensInBrowser) {
        await openHTML(resolved)
        return 'ok'
      }
      return await shell.openPath(resolved) ? 'failed' : 'ok'
    case 'reveal': shell.showItemInFolder(path); return 'ok'
    case 'save': return info.isFile() ? saveAs(window, resolved, basename(path)) : 'unsupported'
    case 'copy_contents': return info.isFile() ? copyContents(resolved) : 'unsupported'
    case 'cursor':
    case 'vscode': {
      const bundle = await editorApp(action)
      if (!bundle) return 'unsupported'
      await run('/usr/bin/open', ['-a', bundle, resolved])
      return 'ok'
    }
  }
}

/** 文件操作仅由聊天主窗口发起，路径必须来自此电脑的会话。 */
export function registerFileLinks(chatWindow: () => BrowserWindow | null, computerId: () => string | undefined) {
  // 主进程校验发起窗口和电脑，防止远程主机或嵌入内容被当成本机。
  ipcMain.handle('fileLinks:act', async (event, id: unknown, path: unknown, action: FileLinkAction): Promise<FileLinkResult> => {
    const window = chatWindow()
    if (!window || event.sender !== window.webContents || event.senderFrame !== event.sender.mainFrame ||
      typeof id !== 'string' || id !== computerId() || typeof path !== 'string' || !actions.has(action)) return 'unsupported'
    try { return await perform(window, path, action) } catch { return 'failed' }
  })
  // 只有点击打开方式后才查询本机编辑器，不随消息渲染查询磁盘。
  ipcMain.handle('fileLinks:editors', async (event, id: unknown): Promise<FileLinkEditor[]> => {
    if (event.sender !== chatWindow()?.webContents || event.senderFrame !== event.sender.mainFrame ||
      typeof id !== 'string' || id !== computerId()) return []
    const installed = await Promise.all((Object.keys(editors) as FileLinkEditor[]).map(async (editor) => await editorApp(editor) ? editor : null))
    return installed.filter((editor) => editor !== null)
  })
}
