import type { Entry, ReplyQuote, SharedFile } from '@shared/models'
import type { HostClient } from '../client/host-client'
import { readSharedFile } from '../../shared/files.ts'
import { prefs } from '../lib/prefs'
import { BotSync } from './bot-sync'

/** A file picked in the composer, sent with the next message. */
export interface OutgoingFile {
  id: string
  name: string
  data: Uint8Array
}

/** What a message carries besides its text; `replyTo` shows on the bubble until the computer's own quote replaces it. */
export interface SendOptions {
  mentions?: import('@shared/mentions').Mention[]
  files?: OutgoingFile[]
  nonce?: string
  pasted?: string[]
  replyTo?: ReplyQuote | null
  /** The voice call it was said in. */
  call?: string | null
}

/** 消息发送、失败重试和附件缓存在同一职责内，主Store只保留聊天操作。 */
export class BotSending extends BotSync {
  private outgoingFiles = new Map<string, OutgoingFile[]>()

  /** The message sent from here last: its row in the queue scrolls into view (`QueuedMessages`). */
  lastSentNonce: string | null = null

  /** 发送成功才清空编辑器；失败和离线路径保留原身份快照。 */
  send(text: string, botId: string, { files = [], nonce = crypto.randomUUID(), pasted = [], mentions = [], replyTo = null, call = null }: SendOptions = {}) {
    const trimmed = text.trim()
    if ((!trimmed && !files.length) || this.retired) return Promise.resolve(false)
    this.lastSentNonce = nonce
    const data: Entry['data'] = { text, mentions, status: 'sending', clientNonce: nonce }
    if (call) data.call = call
    if (replyTo) data.replyTo = replyTo
    if (files.length) {
      data.attachments = files.map((f) => ({ id: f.id, name: f.name, size: f.data.length }))
      this.outgoingFiles.set(nonce, files)
    }
    this.upsert({
      id: `local-${nonce}`, seq: Number.MAX_SAFE_INTEGER, botId, rev: 0, kind: 'user',
      turn: 0, data, createdAt: Date.now(), updatedAt: 0,
    })
    this.changed()
    return (async () => {
      try {
        // Safe to repeat: the computer skips a nonce it already has.
        const e = await this.withLink(async (client) => {
          let ids: string[] | null = null
          const pending = this.outgoingFiles.get(nonce)
          if (pending) {
            ids = []
            for (const file of pending) {
              const id = crypto.randomUUID()
              await client.upload(botId, id, file.name, file.data)
              cacheAttachment(id, file.data)
              ids.push(id)
            }
          }
          return client.send(botId, text, nonce, ids, pasted, replyTo?.id ?? null, call, mentions)
        }, true)
        this.outgoingFiles.delete(nonce)
        this.upsert(e)
        this.changed()
        // Only once it arrived, so a resend counts once.
        if (!call) prefs.promptsSent.set(prefs.promptsSent.get() + 1)
        return true
      } catch (error) {
        if (this.outgoingFiles.has(nonce)) this.lastError = `Couldn't send the files: ${error instanceof Error ? error.message : error}`
        this.lastError = error instanceof Error ? error.message : String(error)
        this.markLocal(nonce, botId, 'failed')
        return false
      }
    })()
  }

  /** 存储故障从 bot 状态恢复，不依赖卡片；操作不自动重放。 */
  retryDeliverySave(botId: string) { return this.perform((c) => c.call('retryDeliverySave', { botId })) }

  /** 主动重试创建新的投递与 attempt，旧记录保留。 */
  retryDelivery(deliveryId: string) { return this.perform((c) => c.call('retryDelivery', { deliveryId })) }

  /** 会话切换由 host 等执行与落盘边界，客户端只发送选择。 */
  switchSession(botId: string, mode: string) { return this.perform((c) => c.call('newSession', { botId, mode })) }

  retry(entry: Entry) {
    const files = (entry.data.clientNonce && this.outgoingFiles.get(entry.data.clientNonce)) || []
    const text = entry.data.text ?? ''
    if (!text && !files.length) return
    this.dropEntry(entry)
    this.send(text, entry.botId, { files, nonce: entry.data.clientNonce, replyTo: entry.data.replyTo, mentions: entry.data.mentions })
  }

  discard(entry: Entry) {
    if (entry.data.clientNonce) this.outgoingFiles.delete(entry.data.clientNonce)
    this.dropEntry(entry)
    this.changed()
    this.scheduleSave()
  }

  private dropEntry(entry: Entry) {
    this.entries.set(entry.botId, (this.entries.get(entry.botId) ?? []).filter((e) => e.id !== entry.id))
  }

  private markLocal(nonce: string, botId: string, status: string) {
    const list = this.entries.get(botId)
    const i = list?.findIndex((e) => e.id === `local-${nonce}`) ?? -1
    if (!list || i < 0) return
    const next = [...list]
    next[i] = { ...next[i]!, data: { ...next[i]!.data, status } }
    this.entries.set(botId, next)
    this.changed()
    this.scheduleSave()
  }

  /** A sent file's bytes: cached after the first fetch (sent files never change). */
  attachmentData(fileId: string, botId: string): Promise<Uint8Array | null> {
    return this.cachedFile(fileId, (client) => client.readUpload(botId, fileId))
  }

  /** A file a bot shared, read whole and checked: cached after the first fetch (shared files never change). */
  sharedFileData(file: SharedFile, entryId: string): Promise<Uint8Array | null> {
    return this.cachedFile(file.id, (client) => readSharedFile(file, (offset) => client.readFile(entryId, file.id, offset)))
  }

  private async cachedFile(id: string, fetch: (client: HostClient) => Promise<Uint8Array>): Promise<Uint8Array | null> {
    const cached = attachmentCache.get(id)
    if (cached) return cached
    const client = this.live
    if (!client) return null
    try {
      const data = await fetch(client)
      cacheAttachment(id, data)
      return data
    } catch {
      return null
    }
  }
}

// ponytail: in-memory attachment cache; a disk cache (main process) if relaunches refetching hurts.
const attachmentCache = new Map<string, Uint8Array>()
function cacheAttachment(id: string, data: Uint8Array) {
  attachmentCache.set(id, data)
}
