import CodyncKit
import Foundation
import os

private let log = Logger(subsystem: "com.pokai.Codync", category: "BotStore")

extension BotStore {
    func composerDraft(for botId: String) -> String {
        composerDrafts[botId] ?? ""
    }

    /// 每次编辑一起更新正文与绑定，碰到片段即解除其旧身份。
    func setComposerDraft(_ text: String, for botId: String, replacing: Range<Int>? = nil) {
        guard !retired else { return }
        if text.isEmpty {
            composerPasted[botId] = nil
        } else if let pasted = pastedInsertion(from: composerDrafts[botId] ?? "", to: text) {
            composerPasted[botId, default: []].append(pasted)
        }
        composerMentions[botId] = Mention.edited(from: composerDrafts[botId] ?? "", to: text, mentions: composerMentions[botId] ?? [], replacement: replacing)
        composerDrafts[botId] = text.isEmpty ? nil : text
        persistComposerDrafts()
    }

    /// 选择成员时把原文与身份一起存入草稿，后续改名不会重新解释这段正文。
    func bindComposerMention(_ mention: Mention, text: String, for key: String) {
        setComposerDraft(text, for: key)
        composerMentions[key, default: []].removeAll { $0.start < mention.end && $0.end > mention.start }
        composerMentions[key, default: []].append(mention)
        persistComposerDrafts()
    }

    /// 主动重试建立新 attempt，旧卡和旧结果始终保留。
    func retryDelivery(_ id: String) {
        Task {
            do {
                try await withLink { client in
                    struct Body: Encodable {var deliveryId: String}
                    let _: Empty = try await client.call("retryDelivery", Body(deliveryId: id))
                }
            } catch { lastError = error.localizedDescription }
        }
    }

    /// 保存内存中的投递结果，不要求存在可用的状态卡片。
    func retryDeliverySave(_ botId: String) {
        Task {
            do {
                try await withLink { client in
                    struct Body: Encodable { var botId: String }
                    let _: Empty = try await client.call("retryDeliverySave", Body(botId: botId))
                }
            } catch { lastError = error.localizedDescription }
        }
    }

    /// 新建会话由 host 等待执行及持久化边界，所有群与主会话共享这一选择。
    func switchSession(_ botId: String, mode: String) {
        Task {
            do {
                try await withLink { client in
                    struct Body: Encodable { var botId: String; var mode: String }
                    let _: Empty = try await client.call("newSession", Body(botId: botId, mode: mode))
                }
            } catch { lastError = error.localizedDescription }
        }
    }

    /// Drafts saved before 2.20.1 were keyed `["<bot>",null]`; a thread's (`["<bot>","<root>"]`) is dropped.
    static func chatDrafts(_ saved: [String: String]) -> [String: String] {
        saved.reduce(into: [:]) { drafts, row in
            guard let old = try? JSONDecoder().decode([String?].self, from: Data(row.key.utf8)) else {
                drafts[row.key] = row.value
                return
            }
            if old.count == 2, let bot = old[0], old[1] == nil { drafts[bot] = row.value }
        }
    }

    /// 同步持久化正文与身份，跨页面和离线均保留绑定。
    func persistComposerDrafts() {
        storage.composerMentions[computer.id] = composerMentions
        storage.composerDrafts[computer.id] = composerDrafts.isEmpty ? nil : composerDrafts
    }

    /// 删除聊天及其附件草稿时一并清理身份，避免残留范围污染后续正文。
    func removeComposerDrafts(for botId: String) {
        composerDrafts = composerDrafts.filter { $0.key != botId && !$0.key.hasPrefix("\(botId)/") }
        composerMentions = composerMentions.filter { $0.key != botId && !$0.key.hasPrefix("\(botId)/") }
        persistComposerDrafts()
    }

    /// An artifact viewer's message box keeps its own draft, apart from the chat's.
    static func artifactDraftKey(_ artifact: Artifact) -> String { "\(artifact.botId)/artifact/\(artifact.id)" }

    /// Transfer to the outbox now; an eventual acknowledgement must not erase a newer draft.
    /// `draft`: whose draft it is (the chat's when nil). `replyTo`: the chat message it quotes.
    @discardableResult
    /// 接受发送后清空本草稿，拒发时恢复身份且不覆盖后续新输入。
    func sendComposerDraft(to botId: String, draft key: String? = nil, replyTo: ReplyQuote? = nil,
                           files: [OutgoingFile] = []) -> Bool {
        let key = key ?? botId
        let text = composerDraft(for: key)
        guard !retired, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !files.isEmpty else { return false }
        let pasted = composerPasted[key] ?? []
        let bindings = composerMentions[key] ?? []
        let sending = send(text, to: botId, replyTo: replyTo, nonce: UUID().uuidString, files: files, pasted: pasted, mentions: bindings)
        setComposerDraft("", for: key)
        // 拒发恢复原草稿键；异步失败不覆盖用户已经继续编辑的内容。
        Task {
            if await sending?.value == .failed, composerDraft(for: key).isEmpty {
                composerDrafts[key] = text
                composerMentions[key] = bindings
                persistComposerDrafts()
            }
        }
        return true
    }

    /// 将本次正文和身份冻结为同一发送记录，重试不重新解析名字。
    public func send(_ text: String, to botId: String, files: [OutgoingFile] = []) {
        send(text, to: botId, replyTo: nil, nonce: UUID().uuidString, files: files)
    }

    /// Said on voice call `call` (on-device or speech to text): the chat shows it in the call's conversation.
    public func send(_ text: String, to botId: String, call: String) {
        send(text, to: botId, replyTo: nil, nonce: UUID().uuidString, call: call)
    }

    @discardableResult
    func send(_ text: String, to botId: String, replyTo: ReplyQuote?, nonce: String, files: [OutgoingFile] = [],
              pasted: [String] = [], call: String? = nil, mentions: [Mention] = []) -> Task<SendProgress, Never>? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !files.isEmpty, !retired else { return nil }
        lastSentNonce = nonce
        var data = EntryData(text: text, status: "sending", clientNonce: nonce)
        data.mentions = mentions
        data.call = call
        data.pasted = pasted.isEmpty ? nil : pasted
        // Shown at once; the computer's own quote replaces it.
        data.replyTo = replyTo
        if !files.isEmpty {
            data.attachments = files.map { Attachment(id: $0.id.uuidString, name: $0.name, size: Int64($0.data.count)) }
            outgoingFiles[nonce] = files
        }
        let local = Entry(
            id: "local-\(nonce)", seq: Int64.max, botId: botId, rev: 0, kind: "user",
            turn: 0, data: data, createdAt: Int64(Date.now.timeIntervalSince1970 * 1000), updatedAt: 0
        )
        upsert(local)
        storage.lastComputerId = computer.id
        if let bot = bots[botId] { onSent?(bot, .sending) }
        // 阶段消息在本机暂存，重连后直接收到可操作的负责人或关闭错误。
        if bots[botId]?.stage != nil, canQueue, files.isEmpty, call == nil {
            markLocal(nonce: nonce, botId: botId, status: "held")
            saveCache()
            return Task { .queued }
        }
        // The relay mailbox holds text only: files, and the call a message was said in, wait for the computer.
        if canQueue, files.isEmpty, call == nil, let remote = transport as? any RemoteTransport {
            return enqueue(remote, text: text, botId: botId, replyTo: replyTo?.id, nonce: nonce, mentions: mentions)
        }
        return deliver(text, botId: botId, replyTo: replyTo?.id, nonce: nonce, pasted: pasted, call: call, mentions: mentions)
    }

    /// 发送已冻结的请求，失败保留可恢复的草稿身份。
    private func deliver(_ text: String, botId: String, replyTo: String?, nonce: String, pasted: [String] = [],
                         call: String? = nil, mentions: [Mention] = []) -> Task<SendProgress, Never> {
        Task {
            do {
                // Safe to repeat: the computer skips a nonce it already has.
                let e = try await withLink(replay: true) { client in
                    var ids: [String]?
                    if let files = self.outgoingFiles[nonce] {
                        // A fresh upload per attempt: a retry never appends to a half-sent file.
                        ids = []
                        for file in files {
                            let id = UUID().uuidString
                            try await client.upload(botId: botId, id: id, name: file.name, data: file.data)
                            await Self.cacheAttachment(id, file.data)
                            ids?.append(id)
                        }
                    }
                    try Task.checkCancellation()
                    return try await client.send(botId: botId, text: text, clientNonce: nonce, replyTo: replyTo, attachments: ids,
                                                 pasted: pasted, call: call, mentions: mentions)
                }
                outgoingFiles[nonce] = nil
                upsert(e)
                // Only once it arrived, so a resend counts once.
                if call == nil { onPromptSent() }
                if let bot = bots[botId] { onSent?(bot, .delivered) }
                return .delivered
            } catch {
                if outgoingFiles[nonce] != nil { lastError = "Couldn't send the files: \(error.localizedDescription)" }
                // 无 relay 的临时掉线留在本机队列，不复制成第二份草稿。
                let holds = (computer.cloud == nil || bots[botId]?.stage != nil) && (error as? HostError)?.isTransient == true
                if !holds { lastError = error.localizedDescription }
                markLocal(nonce: nonce, botId: botId, status: holds ? "held" : "failed")
                return holds ? .queued : .failed
            }
        }
    }

    /// The computer is offline: the message waits, sealed for it, in the relay mailbox.
    /// 离线入队时保留完整正文、引用及结构化提及。
    private func enqueue(_ remote: any RemoteTransport, text: String, botId: String, replyTo: String?, nonce: String, mentions: [Mention] = []) -> Task<SendProgress, Never> {
        markLocal(nonce: nonce, botId: botId, status: "waiting")
        saveCache()
        return Task {
            defer { saveCache() }
            do {
                try Task.checkCancellation()
                try await remote.enqueue(botId: botId, text: text, clientNonce: nonce, replyTo: replyTo, mentions: mentions)
                onPromptSent()
                if let bot = bots[botId] { onSent?(bot, .queued) }
                return .queued
            } catch MailboxError.hostOnline {
                // Presence can race the stream state. Wait for the host's events channel
                // before trying the normal API, instead of turning that race into a failure.
                markLocal(nonce: nonce, botId: botId, status: "sending")
                return await deliverWhenOnline(text, botId: botId, replyTo: replyTo, nonce: nonce, mentions: mentions)
            } catch is CancellationError {
                // The shortcut that sent it was cancelled: the failed message says enough.
                markLocal(nonce: nonce, botId: botId, status: "failed")
                return .failed
            } catch {
                // A dropped relay link: the message's own "Failed to send" and Resend say enough.
                if (error as? HostError)?.isTransient != true { lastError = error.localizedDescription }
                markLocal(nonce: nonce, botId: botId, status: "failed")
                return .failed
            }
        }
    }

    /// 重连后沿用原发送快照，不用改名后的成员重新解释正文。
    private func deliverWhenOnline(_ text: String, botId: String, replyTo: String?, nonce: String, mentions: [Mention] = []) async -> SendProgress {
        let deadline = ContinuousClock.now + .seconds(30)
        while !retired, !Task.isCancelled, connection != .online, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(200))
        }
        guard !retired, !Task.isCancelled else {
            markLocal(nonce: nonce, botId: botId, status: "failed")
            return .failed
        }
        guard connection == .online else {
            lastError = "The computer came online, but the message couldn't be sent. Retry it when the connection is ready."
            markLocal(nonce: nonce, botId: botId, status: "failed")
            saveCache()
            return .failed
        }
        let delivery = deliver(text, botId: botId, replyTo: replyTo, nonce: nonce, mentions: mentions)
        return await withTaskCancellationHandler {
            await delivery.value
        } onCancel: {
            delivery.cancel()
        }
    }

    /// Takes a waiting message back out of the mailbox, unless the computer already has it.
    public func cancelQueued(_ entry: Entry) {
        if computer.cloud == nil || entry.data.status == "held" {
            // No relay holds it: it only waits on this phone.
            discard(entry)
            if let bot = bots[entry.botId] { onSent?(bot, .cancelled) }
            saveCache()
            return
        }
        guard let nonce = entry.data.clientNonce, let remote = transport as? any RemoteTransport else { return }
        Task {
            switch await remote.cancelQueued(clientNonce: nonce) {
            case .cancelled:
                discard(entry)
                if let bot = bots[entry.botId] { onSent?(bot, .cancelled) }
            case .delivering:
                markLocal(nonce: nonce, botId: entry.botId, status: "delivering")
            case .unknown:
                lastError = "Couldn't take the message back. It may have been delivered already."
            }
            saveCache()
        }
    }

    func applyMailbox(_ event: MailboxEvent) {
        guard let entry = entries.values.lazy.flatMap({ $0 }).first(where: { $0.id == "local-\(event.nonce)" }) else { return }
        switch event {
        // The events stream's own copy replaces it (matched by clientNonce).
        case .delivered: markLocal(nonce: event.nonce, botId: entry.botId, status: "delivering")
        case .failed, .expired: markLocal(nonce: event.nonce, botId: entry.botId, status: "failed")
        }
        saveCache()
    }

    /// Messages the relay no longer holds were delivered, refused or expired while we were away.
    /// "Failed" is safe even if one was delivered: the events catch-up replaces it by clientNonce,
    /// and a resend reuses that nonce, so the computer never runs it twice.
    func reconcileQueued(_ remote: any RemoteTransport) async {
        let local = entries.values.flatMap { $0 }
        // What waits on this phone goes now, also when the computer has joined an account since. With no relay
        // now, what a relay held can't be asked about: it goes with the same nonce too.
        sendHeld(local.filter { $0.data.status == "held" || ($0.data.status == "waiting" && computer.cloud == nil) })
        let waiting = local.filter { $0.data.status == "waiting" }
        guard computer.cloud != nil, !waiting.isEmpty else { return }
        let relayItems: [QueuedItem]
        do {
            relayItems = try await remote.listQueued()
        } catch {
            log.debug("mailbox reconciliation deferred: \(error.localizedDescription)")
            return
        }
        let inRelay = Dictionary(relayItems.map { ($0.nonce, $0.state) }, uniquingKeysWith: { a, _ in a })
        guard !retired else { return }
        for e in waiting {
            guard let nonce = e.data.clientNonce else { continue }
            switch inRelay[nonce] {
            case "queued": break
            case .some: markLocal(nonce: nonce, botId: e.botId, status: "delivering")
            case nil: markLocal(nonce: nonce, botId: e.botId, status: "failed")
            }
        }
        saveCache()
    }

    /// 恢复本机暂存时核验附件仍存在，避免丢失文件后误发纯文本。
    private func sendHeld(_ held: [Entry]) {
        guard !held.isEmpty else { return }
        for entry in held.sorted(by: { $0.createdAt < $1.createdAt }) {
            if filesGone(entry), let nonce = entry.data.clientNonce {
                markLocal(nonce: nonce, botId: entry.botId, status: "failed")
            } else {
                retry(entry)
            }
        }
        saveCache()
    }

    /// 恢复本机暂存时核验附件仍存在，避免丢失文件后误发纯文本。
    private func filesGone(_ entry: Entry) -> Bool {
        entry.data.attachments?.isEmpty == false && entry.data.clientNonce.flatMap { outgoingFiles[$0] } == nil
    }

    /// 重发同一请求身份，区别于投递卡主动创建新 attempt。
    public func retry(_ entry: Entry) {
        guard !filesGone(entry) else {
            lastError = "This message's files aren't on this device anymore. Delete it and attach them again."
            return
        }
        let files = entry.data.clientNonce.flatMap { outgoingFiles[$0] } ?? []
        guard let text = entry.data.text, !text.isEmpty || !files.isEmpty else { return }
        entries[entry.botId]?.removeAll { $0.id == entry.id }
        // The same clientNonce: if the computer got it after all, it won't run twice (the host
        // skips a nonce it has). Every seal still takes a fresh ephemeral key (§6.4).
        let sending = send(text, to: entry.botId, replyTo: entry.data.replyTo, nonce: entry.data.clientNonce ?? UUID().uuidString,
                           files: files, pasted: entry.data.pasted ?? [], call: entry.data.call, mentions: entry.data.mentions ?? [])
        if entry.data.status == "held", bots[entry.botId]?.stage != nil {
            // 暂存阶段请求遭到拒绝时恢复可编辑正文和选人身份，保留更新的输入。
            Task {
                if await sending?.value == .failed, composerDraft(for: entry.botId).isEmpty {
                    composerDrafts[entry.botId] = text
                    composerMentions[entry.botId] = entry.data.mentions
                    persistComposerDrafts()
                }
            }
        }
    }

    /// 已发送附件通过同一有界读取队列获取，缓存只保存原始字节。
    public func attachmentData(_ file: Attachment, bot botId: String) async -> Data? {
        await cachedAttachment(file.id) { [weak self] in
            guard let self, let client = live else { return nil }
            return try? await client.readUpload(botId: botId, id: file.id)
        }
    }

    /// 显式分享的图片仍通过原消息身份读取并核对 SHA-256。
    public func sharedFileData(_ file: SharedFile, entryId: String) async -> Data? {
        await cachedAttachment(file.id) { [weak self] in
            guard let self, let client = live else { return nil }
            return try? await FileDownload().data(client: client, entryId: entryId, file: file)
        }
    }

    /// 相同图片共用一次读取；队列只保留完成信号，避免持有上一张图的字节。
    private func cachedAttachment(_ id: String, load: @escaping @MainActor () async -> Data?) async -> Data? {
        guard !retired else { return nil }
        if let prior = attachmentReads[id] {
            let data = await prior.value
            return Task.isCancelled ? nil : data
        }
        let previous = attachmentReadTail
        // 共享任务属于电脑 Store，单个视图取消只忽略自己的等待结果。
        let request = Task { [weak self] in
            await previous?.value
            guard let self, !retired, !Task.isCancelled else { return Optional<Data>.none }
            // 磁盘读取明确离开 MainActor，不阻塞原图手势或聊天刷新。
            let cached = await Task.detached(priority: .utility) {
                guard let url = Self.attachmentCache(id) else { return Optional<Data>.none }
                return try? Data(contentsOf: url)
            }.value
            guard !retired, !Task.isCancelled else { return nil }
            if let cached { return cached }
            guard let data = await load(), !retired, !Task.isCancelled else { return nil }
            await Self.cacheAttachment(id, data)
            return !retired && !Task.isCancelled ? data : nil
        }
        attachmentReads[id] = request
        // 只保留完成信号；共享下载由 Store 拥有，不因一个视图离屏取消其他等待者。
        attachmentReadTail = Task { _ = await request.value }
        defer { attachmentReads[id] = nil }
        let data = await request.value
        return Task.isCancelled ? nil : data
    }

    /// 缓存只用已验证的 UUID，不能把远端名称当作本机路径。
    nonisolated private static func attachmentCache(_ id: String) -> URL? {
        guard let uuid = UUID(uuidString: id) else { return nil }
        return URL.cachesDirectory.appending(path: "codync-attachments").appending(path: uuid.uuidString)
    }

    /// 磁盘缓存最多约 512 MiB，保留当前文件以兼容既有大附件。
    private static func cacheAttachment(_ id: String, _ data: Data) async {
        // 写入与裁剪在后台执行；Store 退场取消时跳过后续操作，已进入的系统写入可能完成。
        let worker = Task.detached(priority: .utility) { writeAttachmentCache(id, data) }
        await withTaskCancellationHandler { /* 等待后台原子写入与裁剪完成。 */ await worker.value } onCancel: { /* Store 取消时停止后续磁盘操作。 */ worker.cancel() }
    }

    /// UUID 路径、原子写入和缓存淘汰均为纯文件操作，不能读取 Store 状态。
    nonisolated private static func writeAttachmentCache(_ id: String, _ data: Data) {
        guard let url = attachmentCache(id) else { return }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        guard !Task.isCancelled else { return }
        try? data.write(to: url, options: .atomic)
        let folder = url.deletingLastPathComponent()
        let files = (try? FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey])) ?? []
        let ordered = files.compactMap { file -> (URL, Int, Date)? in
            // 仅淘汰本模块生成的 UUID 文件，忽略其他缓存条目。
            guard UUID(uuidString: file.lastPathComponent) != nil,
                  let values = try? file.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey]) else { return nil }
            return (file, values.fileSize ?? 0, values.contentModificationDate ?? .distantPast)
        }.sorted { /* 优先淘汰最旧的磁盘副本。 */ $0.2 < $1.2 }
        var total = ordered.reduce(0) { /* 汇总缓存字节，用于软额度裁剪。 */ $0 + $1.1 }
        for (file, size, _) in ordered where file != url && total > 512 * 1024 * 1024 {
            if Task.isCancelled { break }
            if (try? FileManager.default.removeItem(at: file)) != nil { total -= size }
        }
    }

    public func discard(_ entry: Entry) {
        if let nonce = entry.data.clientNonce { outgoingFiles[nonce] = nil }
        entries[entry.botId]?.removeAll { $0.id == entry.id }
        scheduleSave()
    }

    private func markLocal(nonce: String, botId: String, status: String) {
        guard var list = entries[botId], let i = list.firstIndex(where: { $0.id == "local-\(nonce)" }) else { return }
        list[i].data.status = status
        entries[botId] = list
        scheduleSave()
        if status == "failed", let bot = bots[botId] { onSent?(bot, .failed) }
    }
}
