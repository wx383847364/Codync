import CodyncKit
import SwiftUI

/// 暂停后保留任务身份和摘要，用户仍能在原群继续。
struct HeldDeliveryRow: View {
    let chat: Bot
    let task: GroupDeliveryControl
    @Environment(BotStore.self) private var model

    /// 等待重新执行时显示状态，收到新的开始事件后由活动标签接管。
    var body: some View {
        let paused = task.status == "paused"
        let status = L10n.tr(paused ? "Paused" : "Continuing…")
        let name = model.bots[task.targetBotId]?.name ?? L10n.tr("Bot")
        HStack(spacing: 8) {
            Image(systemName: paused ? "pause.fill" : "clock")
                .font(.footnote).accessibilityHidden(true)
            Text("\(name) · \(status)" + (task.preview.isEmpty ? "" : " · \(task.preview)"))
                .font(.subheadline).lineLimit(1)
            if paused {
                IconButton("Continue", systemImage: "play.fill") {
                    // 继续同一个派工，Host 负责安排空闲时执行。
                    model.setDeliveryPaused(chat.id, task: task, paused: false)
                }
                .disabled(chat.stage?.closedAt != nil || chat.stage?.archivedAt != nil)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, paused ? 4 : 12)
        .foregroundStyle(Palette.secondary)
        .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
    }
}

/// 默认一行中文状态，展开仅显示成员状态和重试，不重复来源正文。
struct DeliveryNoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model
    @State private var expanded = false

    /// 折叠状态属于本地展示，状态更新和重试仍由 host 管理。
    var body: some View {
        Disclosure(isExpanded: $expanded) {
            Text("第 \(entry.data.deliveryAttempt ?? 1) 次投递" + (entry.data.parentDeliveryId != nil ? " · 重试投递" : ""))
                .font(.footnote).foregroundStyle(Palette.secondary)
            ForEach(entry.data.deliveryRows ?? [], id: \.deliveryId) { row in
                // 显式重试交给 host 创建新投递，不覆盖旧记录。
                HStack(spacing: 8) {
                    Text("\(model.bots[row.targetBotId]?.name ?? "已移除成员")：\(row.statusText)" + (row.detail.isEmpty ? "" : " · \(row.detail)"))
                        .font(.footnote).foregroundStyle(Palette.secondary)
                    if ["failed", "interrupted", "noReply"].contains(row.status) {
                        Button("重新尝试") { model.retryDelivery(row.deliveryId) }.buttonStyle(.secondary)
                    }
                }
            }
        } label: {
            Text(DeliveryRow.summary(entry.data.deliveryRows ?? []))
                .font(.footnote).foregroundStyle(Palette.secondary).lineLimit(1)
        }
    }
}
