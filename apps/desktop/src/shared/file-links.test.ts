import assert from 'node:assert/strict'
import * as fs from 'node:fs'
import * as files from 'node:fs/promises'
import * as crypto from 'node:crypto'
import * as child from 'node:child_process'
import * as streams from 'node:stream/promises'
import * as util from 'node:util'
import * as url from 'node:url'
import { EventEmitter, once } from 'node:events'
import path from 'node:path'
import { tmpdir } from 'node:os'
import { test } from 'node:test'
import type { BrowserWindow } from 'electron'
import { fileLinkPath, isHTMLFileLink } from './file-links.ts'
import { loadMainModule } from './test-support/main-module.ts'

/** 文件目标在各系统的聊天里可复制，网络路径和危险字符没有本地动作。 */
test('file links decode absolute POSIX and Windows paths without treating URLs as files', () => {
  assert.equal(fileLinkPath('/Users/bruce/Desktop/My%20App.app'), '/Users/bruce/Desktop/My App.app')
  assert.equal(fileLinkPath('file:///C:/work/a%2Bb.png'), 'C:/work/a+b.png')
  assert.equal(fileLinkPath('C:\\work\\photo.png'), 'C:\\work\\photo.png')
  assert.equal(fileLinkPath('file://localhost/tmp/%E5%9B%BE.png'), '/tmp/图.png')
  for (const invalid of ['C:work/photo.png', 'relative/path', '//server/share', '/%2Fserver/share', 'file://server/share', 'file://localhost//server/share', '/tmp/%00name', 'https://example.com/photo.png']) {
    assert.equal(fileLinkPath(invalid), null, invalid)
  }
})

/** 验证真实文件的操作、另存权限及 IPC 边界，系统菜单和剪贴板使用隔离替身。 */
test('local file actions read and save regular files only from the chat window', async () => {
  const folder = await files.mkdtemp(path.join(tmpdir(), 'codync-file-link-main-'))
  const source = path.join(folder, 'A report.txt')
  const dest = path.join(folder, 'Saved report.txt')
  const binary = path.join(folder, 'image.png')
  const mainFrame = {}
  const sender = { mainFrame }
  const origin = { sender, senderFrame: mainFrame }
  const window = { webContents: sender } as unknown as BrowserWindow
  let copied = ''
  let opened = ''
  const handlers = new Map<string, (...args: unknown[]) => unknown>()
  const electron = {
    ipcMain: {
      // 截获真实主进程注册的回调，复用其参数校验。
      handle: (name: string, fn: (...args: unknown[]) => unknown) => handlers.set(name, fn),
    },
    clipboard: {
      // 记录剪贴板文字，避免触碰开发设备的剪贴板。
      writeText: (text: string) => { copied = text },
    },
    shell: {
      // 记录交给默认应用的规范化路径。
      openPath: async (target: string) => { opened = target; return '' },
    },
    dialog: {
      // 模拟用户在系统面板选择唯一的临时目标。
      showSaveDialog: async () => ({ canceled: false, filePath: dest }),
    },
  }
  const { registerFileLinks } = loadMainModule<typeof import('../main/file-links.ts')>('file-links', {
    'node:child_process': child, 'node:crypto': crypto, 'node:events': { once }, 'node:fs': fs, 'node:fs/promises': files,
    'node:path': path, 'node:stream/promises': streams, 'node:util': util, 'node:url': url, electron,
    '../shared/file-links': { isHTMLFileLink },
  }, { process, Buffer, TextDecoder })
  // 当前电脑和聊天窗口由主进程提供。
  registerFileLinks(() => window, () => 'local')
  const action = handlers.get('fileLinks:act')!
  try {
    await files.writeFile(source, 'Report with Unicode 报告\n', { mode: 0o600 })
    await files.writeFile(binary, Buffer.from([0x89, 0x50, 0x4e, 0x47, 0]))
    assert.equal(await action(origin, 'local', source + ':12:4', 'copy_contents'), 'ok')
    assert.equal(copied, 'Report with Unicode 报告\n')
    assert.equal(await action(origin, 'local', binary, 'copy_contents'), 'unsupported')
    assert.equal(await action(origin, 'local', source, 'save'), 'ok')
    assert.deepEqual(await files.readFile(dest), await files.readFile(source))
    if (process.platform !== 'win32') assert.equal((await files.stat(dest)).mode & 0o777, 0o600)
    assert.deepEqual((await files.readdir(folder)).sort(), ['A report.txt', 'Saved report.txt', 'image.png'])
    assert.equal(await action(origin, 'local', source, 'open'), 'ok')
    assert.equal(opened, await files.realpath(source))
    assert.equal(await action(origin, 'local', path.join(folder, 'missing.txt'), 'open'), 'missing')
    assert.equal(await action(origin, 'remote', source, 'open'), 'unsupported')
    assert.equal(await action({ sender, senderFrame: {} }, 'local', source, 'open'), 'unsupported')
  } finally {
    await files.rm(folder, { recursive: true, force: true })
  }
})

/** HTML 直接交给已注册的默认浏览器。模拟各系统的启动器，避免抢占用户窗口。 */
test('HTML opens in the HTTPS browser on each desktop platform', async () => {
  assert.equal(isHTMLFileLink('/tmp/Report.HTM'), true)
  assert.equal(isHTMLFileLink('/tmp/Report.html.txt'), false)
  for (const variant of ['darwin', 'win32', 'linux', 'linux-fallback'] as const) {
    const fallback = variant === 'linux-fallback'
    const platform = variant === 'linux-fallback' ? 'linux' : variant
    const win = platform === 'win32'
    const html = win ? 'C:\\Reports\\Example #1.html' : '/Reports/Example #1.html'
    const htmlURL = win ? 'file:///C:/Reports/Example%20%231.html' : 'file:///Reports/Example%20%231.html'
    const browser = win ? 'C:\\Program Files\\Browser\\browser.exe' : platform === 'darwin' ? '/Applications/Browser.app' : '/usr/bin/browser'
    const opened: string[][] = []
    const mainFrame = {}
    const sender = { mainFrame }
    const handlers = new Map<string, (...args: unknown[]) => unknown>()
    const electron = {
      shell: {
        // 保留带行号的 HTML 源码链接使用系统文件关联的行为。
        openPath: async (target: string) => { opened.push(['openPath', target]); return '' },
      },
      ipcMain: {
        // 捕获实际文件动作入口，使用生产参数校验。
        handle: (name: string, fn: (...args: unknown[]) => unknown) => handlers.set(name, fn),
      },
      app: {
        // 从系统的 HTTPS 关联取得已安装应用。
        getApplicationInfoForProtocol: async (protocol: string) => {
          assert.equal(new URL(protocol).protocol, 'https:')
          return { path: browser }
        },
        // Linux 的启动器测试指定用户数据目录。
        getPath: () => '/home/test',
      },
    }
    const { registerFileLinks } = loadMainModule<typeof import('../main/file-links.ts')>('file-links', {
      electron, 'node:fs': fs, 'node:crypto': crypto, 'node:events': { once }, 'node:stream/promises': streams,
      '../shared/file-links': { isHTMLFileLink },
      'node:path': win ? path.win32 : path.posix,
      'node:url': {
        // 在当前测试系统上也能生成目标平台的文件 URL。
        pathToFileURL: (target: string) => url.pathToFileURL(target, { windows: win }),
      },
      'node:fs/promises': {
        // 只返回夹具路径和已安装的桌面条目。
        realpath: async (target: string) => {
          // 源码引用的行号后缀不属于实际文件名。
          if (target === html + ':12') throw Object.assign(new Error('Missing'), { code: 'ENOENT' })
          return target
        },
        // Linux 默认 ID 来自 XDG 子目录，验证它能解析成实际启动项。
        stat: async (target: string) => ({
          isFile: () => target === html || target === '/home/test/.local/share/applications/vendor/browser.desktop',
          isDirectory: () => target === '/home/test/.local/share/applications/vendor',
        }),
      },
      'node:util': {
        // 记录 macOS 和 Linux 的无 shell 启动参数。
        promisify: () => async (program: string, args: string[]) => {
          opened.push([program, ...args])
          if (fallback) throw Object.assign(new Error('Missing tool'), { code: 'ENOENT' })
          return { stdout: 'vendor-browser.desktop\n' }
        },
      },
      'node:child_process': {
        // 模拟 GIO 启动器和直接浏览器进程的不同生命周期。
        spawn: (program: string, args: string[]) => {
          opened.push([program, ...args])
          const proc = Object.assign(new EventEmitter(), { unref() {} })
          // 模拟子进程异步启动。
          queueMicrotask(() => {
            // GIO 结束后浏览器继续运行。直接启动时只等进程就绪。
            if (program === 'gio') proc.emit('close', 0)
            else proc.emit('spawn')
          })
          return proc
        },
      },
    }, { process: { platform, env: {} } })
    // 真实 IPC 只允许本机聊天窗口调用。
    registerFileLinks(() => ({ webContents: sender } as unknown as BrowserWindow), () => 'local')
    const action = handlers.get('fileLinks:act')!
    assert.equal(await action({ sender, senderFrame: mainFrame }, 'local', html, 'open'), 'ok')
    if (win) assert.deepEqual(opened, [[browser, htmlURL]])
    else if (platform === 'darwin') assert.deepEqual(opened, [['/usr/bin/open', '-a', browser, html]])
    else if (fallback) assert.deepEqual(opened, [['xdg-mime', 'query', 'default', 'x-scheme-handler/https'], [browser, htmlURL]])
    else assert.deepEqual(opened, [
      ['xdg-mime', 'query', 'default', 'x-scheme-handler/https'],
      ['gio', 'launch', '/home/test/.local/share/applications/vendor/browser.desktop', htmlURL],
    ])
    opened.length = 0
    assert.equal(await action({ sender, senderFrame: mainFrame }, 'local', html + ':12', 'open'), 'ok')
    assert.deepEqual(opened, [['openPath', html]])
  }
})
