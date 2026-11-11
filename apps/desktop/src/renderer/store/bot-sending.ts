import { AttachmentCache } from './attachment-cache.ts'
import type { Entry, ReplyQuote, SharedFile } from '@shared/models'
import type { HostClient } from '../client/host-client'
import { readSharedFile } from '../../shared/files.ts'
import { prefs } from '../lib/prefs'
import { BotSync } from './bot-sync'

import { filesToResend, type OutgoingFile } from './outgoing-files.ts'
import { trNow } from '../lib/l10n.ts'
export type { OutgoingFile } from './outgoing-files.ts'

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
  private attachmentCache = new AttachmentCache()

  /** 电脑退场时释放附件缓存，防止其他账号继续使用已排队的读取。 */
  override retire() { this.attachmentCache.retire(); super.retire() }

  private outgoingFiles = new Map<string, OutgoingFile[]>()

  /** The message sent from here last: its row in the queue scrolls into view (`QueuedMessages`). */
  lastSentNonce: string | null = null

  /** 发送成功才清空编辑器；失败和离线路径保留原身份快照。 */
  send(text: string, botId: string, { files = [], nonce = crypto.randomUUID(), pasted = [], mentions = [], replyTo = null, call = null }: SendOptions = {}) {
    const trimmed = text.trim()
    if ((!trimmed && !files.length) || this.retired) return Promise.resolve(false)
    this.lastSentNonce = nonce
    const data: Entry['data'] = { text, mentions, pasted, status: 'sending', clientNonce: nonce }
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
        // 发送回调保持重试身份；上传完成的字节只进入有界缓存。
        const e = await this.withLink(async (client) => {
          let ids: string[] | null = null
          const pending = this.outgoingFiles.get(nonce)
          if (pending) {
            ids = []
            for (const file of pending) {
              const id = crypto.randomUUID()
              await client.upload(botId, id, file.name, file.data)
              this.attachmentCache.put(id, file.data)
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

  /** 重试保留原身份；本机附件丢失时拒绝发送，不能悄悄降为纯文本。 */
  retry(entry: Entry) {
    const files = filesToResend(entry, this.outgoingFiles)
    if (!files) return this.setError(trNow("This message's files aren't on this device anymore. Delete it and attach them again."))
    const text = entry.data.text ?? ''
    if (!text && !files.length) return
    this.dropEntry(entry)
    this.send(text, entry.botId, { files, nonce: entry.data.clientNonce, replyTo: entry.data.replyTo, pasted: entry.data.pasted, call: entry.data.call, mentions: [...entry.data.mentions ?? [], ...entry.data.workspaceFileReferences ?? []] })
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

  /** 获取已发送附件的有界缓存，旧主机不调用新图片检查接口。 */
  attachmentData(fileId: string, botId: string): Promise<Uint8Array | null> {
    return this.cachedFile(fileId, /* 读取已上传附件，不访问本机路径。 */ (client) => client.readUpload(botId, fileId), { botId, uploadId: fileId })
  }

  /** 获取显式分享的图片，先核验归属和尺寸，再验证完整字节摘要。 */
  sharedFileData(file: SharedFile, entryId: string): Promise<Uint8Array | null> {
    return this.cachedFile(file.id, /* 按消息分页读取并验证摘要。 */ (client) => readSharedFile(file, (offset) => client.readFile(entryId, file.id, offset)), { entryId, fileId: file.id })
  }

  /** 先使用本电脑已有缓存以保留离线预览；首次下载才检查附件归属与尺寸。 */
  private async cachedFile(id: string, fetch: (client: HostClient) => Promise<Uint8Array>, source: object): Promise<Uint8Array | null> {
    try {
      return await this.attachmentCache.read(id, async () => {
        // 等到下载槽位可用时再取连接；缓存命中不依赖主机在线。
        const client = this.live
        if (!client || this.retired) return null
        if (this.hello?.fileImageInfo) await client.call('imageInfo', source)
        if (this.retired || this.live !== client) return null
        return fetch(client)
      })
    } catch { return null }
  }
}
