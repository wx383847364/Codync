import assert from 'node:assert/strict'
import * as crypto from 'node:crypto'
import { EventEmitter } from 'node:events'
import * as fs from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import type { BrowserWindow } from 'electron'
import { FileSink } from '../main/file-sink.ts'
import * as sharedFiles from './files.ts'
import * as fileLinks from './file-links.ts'
import { loadMainModule } from './test-support/main-module.ts'

test('file card actions use the verified download and never an original path', async () => {
  const folder = await fs.mkdtemp(path.join(tmpdir(), 'codync-shared-file-main-'))
  const destination = path.join(folder, 'saved report.md')
  const bytes = Buffer.from('# Shared report 报告\n')
  const file = { id: 'file', name: 'report.md', size: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') }
  const sender = Object.assign(new EventEmitter(), { id: 1, mainFrame: {}, isDestroyed: () => false })
  const origin = { sender, senderFrame: sender.mainFrame }
  const window = { webContents: sender } as unknown as BrowserWindow
  const nativeActions: string[] = []
  const handlers = new Map<string, (...args: unknown[]) => unknown>()
  let copied = ''
  let cancelled = false
  const { registerFileSaves } = loadMainModule<typeof import('../main/file-saves.ts')>('file-saves', {
    'node:crypto': crypto, 'node:fs/promises': fs, 'node:path': path, '../shared/files': sharedFiles,
    './file-sink': { FileSink }, '../shared/file-links': fileLinks,
    './file-links': {
      actions: new Set(['open', 'save', 'reveal', 'copy_contents']),
      isNativePath: path.isAbsolute,
      installedEditors: async () => ['cursor'],
      performFileAction: async (_window: BrowserWindow, source: string, action: string, start: () => boolean) => {
        assert.equal(path.basename(source), file.name)
        assert.deepEqual(await fs.readFile(source), bytes)
        if (!start()) return 'cancelled'
        nativeActions.push(action)
        if (action === 'copy_contents') copied = bytes.toString()
        return 'ok'
      },
    },
    electron: {
      app: { getPath: () => folder },
      ipcMain: { handle: (name: string, fn: (...args: unknown[]) => unknown) => handlers.set(name, fn) },
      clipboard: { writeText: (text: string) => { copied = text } },
      dialog: { showSaveDialog: async () => ({ canceled: cancelled, filePath: cancelled ? undefined : destination }) },
    },
  }, { process })
  registerFileSaves(() => window)
  const begin = handlers.get('files:begin')!
  const write = handlers.get('files:write')!
  const finish = handlers.get('files:finish')!
  try {
    await assert.rejects(async () => begin({ sender, senderFrame: {} }, file, 'open'), /Window closed/)
    cancelled = true
    assert.equal(await begin(origin, file, 'save'), null)
    cancelled = false
    const broken = await begin(origin, file, 'open')
    await assert.rejects(async () => finish(origin, broken), /integrity check failed/i)
    assert.deepEqual(await fs.readdir(path.join(folder, 'shared-file-cache')), [])

    for (const action of ['copy_contents', 'save', 'open', 'reveal', 'copy_path']) {
      const handle = await begin(origin, file, action)
      await write(origin, handle, 0, bytes)
      assert.equal(await finish(origin, handle), 'ok')
      if (action === 'copy_contents') {
        assert.equal(copied, bytes.toString())
        assert.deepEqual(await fs.readdir(path.join(folder, 'shared-file-cache')), [])
      }
    }
    assert.deepEqual(nativeActions, ['copy_contents', 'open', 'reveal'])
    assert.deepEqual(await fs.readFile(destination), bytes)
    assert.deepEqual(await fs.readFile(copied), bytes)
  } finally {
    await fs.rm(folder, { recursive: true, force: true })
  }
})
