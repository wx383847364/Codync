import { randomUUID } from 'node:crypto'
import { mkdir, mkdtemp, readdir, rm, stat } from 'node:fs/promises'
import { join } from 'node:path'
import { app, clipboard, dialog, ipcMain, type BrowserWindow, type IpcMainInvokeEvent } from 'electron'
import { FILE_TEXT_LIMIT, FILE_TEXT_LIMIT_MESSAGE, canOpenSharedFile, type FileLinkResult, type SharedFileAction } from '../shared/file-links'
import type { SharedFile } from '../shared/models'
import { saveName, validateSharedFile } from '../shared/files'
import { actions, installedEditors, isNativePath, performFileAction } from './file-links'
import { FileSink } from './file-sink'

interface Download {
  owner: number
  sink: FileSink
  dispose: () => void
  destination: string
  temporary: string | null
  action: SharedFileAction
  finishing: boolean
}

export function registerFileSaves(chatWindow: () => BrowserWindow | null) {
  const saves = new Map<string, Download>()
  let storage: Promise<string> | undefined

  function chatSender(event: IpcMainInvokeEvent) {
    const window = chatWindow()
    if (event.sender === window?.webContents && event.senderFrame === event.sender.mainFrame) return window
    throw new Error('Window closed')
  }

  ipcMain.handle('files:editors', (event) => {
    chatSender(event)
    return installedEditors()
  })

  ipcMain.handle('files:begin', async (event, meta: SharedFile, action: SharedFileAction = 'save') => {
    const window = chatSender(event)
    validateSharedFile(meta)
    if (action !== 'copy_path' && !actions.has(action)) throw new Error('Invalid file action')
    if (action === 'copy_contents' && meta.size > FILE_TEXT_LIMIT) throw new Error(FILE_TEXT_LIMIT_MESSAGE)

    const id = randomUUID()
    const name = saveName(meta.name)
    // A system action gets its own local copy. An editor can change it without changing the snapshot
    // or overwriting a file the user already saved.
    let temporary: string | null = null
    if (action !== 'save') {
      storage ??= prepareStorage().catch((error) => { storage = undefined; throw error })
      temporary = await mkdtemp(join(await storage, 'codync-shared-file-'))
    }
    try {
      const destination = temporary
        ? join(temporary, name)
        : await saveDestination(window, name)
      if (!destination || event.sender.isDestroyed()) {
        await removeTemporary(temporary)
        return null
      }
      if (temporary && !isNativePath(destination)) throw new Error('Invalid filename')
      const sink = await FileSink.create(destination, meta, id)
      if (event.sender.isDestroyed()) {
        await sink.cancel()
        await removeTemporary(temporary)
        return null
      }
      const destroyed = () => {
        saves.delete(id)
        void discard(sink, temporary).catch(console.error)
      }
      saves.set(id, { owner: event.sender.id, sink, destination, temporary, action, finishing: false,
        dispose: () => event.sender.removeListener('destroyed', destroyed) })
      event.sender.once('destroyed', destroyed)
      return id
    } catch (error) {
      await removeTemporary(temporary)
      throw error
    }
  })

  function owned(owner: number, id: string) {
    const save = saves.get(id)
    if (!save || save.owner !== owner) throw new Error('Unknown download')
    return save
  }
  function forget(id: string) { saves.get(id)?.dispose(); saves.delete(id) }

  ipcMain.handle('files:write', async (event, id: string, offset: number, bytes: Uint8Array) => {
    const save = owned(event.sender.id, id)
    if (save.finishing) throw new Error('Download is finishing')
    try { await save.sink.write(offset, bytes) } catch (error) {
      forget(id)
      await discard(save.sink, save.temporary)
      throw error
    }
  })

  ipcMain.handle('files:finish', async (event, id: string): Promise<FileLinkResult> => {
    const window = chatSender(event)
    const save = owned(event.sender.id, id)
    if (save.finishing) throw new Error('Download is finishing')
    save.finishing = true
    const { sink, action, destination, temporary } = save
    let keep = false
    try {
      await sink.finish()
      // Cancellation or window destruction during the transfer suppresses the pending system action.
      if (!saves.has(id) || event.sender.isDestroyed()) return 'cancelled'
      if (action === 'save') return 'ok'
      if (action === 'open' && !canOpenSharedFile(destination, process.platform)) return 'unsupported'
      // Claim the completed file just before the native action starts. Until then a cancelled
      // transfer or closed window can still stop it and remove the temporary copy.
      const start = () => {
        if (!saves.has(id) || event.sender.isDestroyed()) return false
        forget(id)
        return true
      }
      if (action === 'copy_path') {
        if (!start()) return 'cancelled'
        clipboard.writeText(destination)
        keep = true
        return 'ok'
      }
      const result = await performFileAction(window, destination, action, start)
      keep = result === 'ok' && action !== 'copy_contents'
      return result
    } finally {
      forget(id)
      await discard(sink, keep ? null : temporary)
    }
  })

  ipcMain.handle('files:cancel', async (event, id: string) => {
    if (!saves.has(id)) return
    const { sink, temporary } = owned(event.sender.id, id)
    forget(id)
    await discard(sink, temporary)
  })
}

const DAY = 24 * 60 * 60 * 1000

/** Old copies are removed before the first transfer, while no new download can be using them. */
async function prepareStorage() {
  const root = join(app.getPath('userData'), 'shared-file-cache')
  await mkdir(root, { recursive: true, mode: 0o700 })
  const expiry = Date.now() - DAY
  for (const entry of await readdir(root, { withFileTypes: true })) {
    if (!entry.isDirectory() || !entry.name.startsWith('codync-shared-file-')) continue
    const folder = join(root, entry.name)
    try {
      if ((await stat(folder)).mtimeMs < expiry) await removeTemporary(folder)
    } catch (error) { console.error('Could not remove a temporary shared file', error) }
  }
  return root
}

async function saveDestination(window: BrowserWindow, name: string) {
  const result = await dialog.showSaveDialog(window, { defaultPath: name, buttonLabel: 'Save' })
  return result.canceled ? undefined : result.filePath
}

async function removeTemporary(folder: string | null) {
  if (folder) await rm(folder, { recursive: true, force: true })
}

async function discard(sink: FileSink, folder: string | null) {
  try { await sink.cancel() } finally { await removeTemporary(folder) }
}
