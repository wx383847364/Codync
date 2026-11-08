/** 图片字节缓存有总量上限，读取共享两个下载槽，避免长聊天同时下载所有原图。 */
export class AttachmentCache {
  private bytes = new Map<string, Uint8Array>()
  private pending = new Map<string, Promise<Uint8Array | null>>()
  private waiting: (() => void)[] = []
  private running = 0
  private used = 0
  private retired = false

  /** 更新最近使用顺序并在超出 64 MiB 时淘汰最旧字节。 */
  put(id: string, data: Uint8Array) {
    if (this.retired) return
    this.used -= this.bytes.get(id)?.byteLength ?? 0
    this.bytes.delete(id)
    this.bytes.set(id, data)
    this.used += data.byteLength
    while (this.used > 64 * 1024 * 1024 && this.bytes.size) {
      const first = this.bytes.keys().next().value!
      this.used -= this.bytes.get(first)!.byteLength
      this.bytes.delete(first)
    }
  }

  /** 相同文件共享一次读取；缓存淘汰只影响下次请求，不影响已显示图片。 */
  async read(id: string, fetch: () => Promise<Uint8Array | null>): Promise<Uint8Array | null> {
    if (this.retired) return null
    const cached = this.bytes.get(id)
    if (cached) { this.put(id, cached); return cached }
    const pending = this.pending.get(id)
    if (pending) return pending
    const job = this.load(id, fetch)
    this.pending.set(id, job)
    try { return await job } finally { this.pending.delete(id) }
  }

  /** 等待下载槽；释放时直接移交给下一请求，不产生并发窗口。 */
  private async load(id: string, fetch: () => Promise<Uint8Array | null>) {
    if (this.running >= 2) await new Promise<void>((resolve) => { /* 下载槽释放时唤醒下一个等待者。 */ this.waiting.push(resolve) })
    else this.running++
    try {
      if (this.retired) return null
      const data = await fetch()
      if (this.retired) return null
      if (data) this.put(id, data)
      return data
    } finally {
      const next = this.waiting.shift()
      if (next) next()
      else this.running--
    }
  }
  /** 账号或电脑退场后释放字节，排队请求不再发起读取，晚到响应不重新填充缓存。 */
  retire() { this.retired = true; this.bytes.clear(); this.used = 0 }
}
