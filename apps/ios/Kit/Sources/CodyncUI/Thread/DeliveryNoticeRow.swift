import CodyncKit
import SwiftUI

/// 来源事件和 attempt 聚合卡，成员改名只改变目标行，原正文始终保留。
struct DeliveryNoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Attempt \(entry.data.deliveryAttempt ?? 1)" + (entry.data.parentDeliveryId.map { " · retry of \($0.prefix(8))" } ?? "")).font(.footnote).foregroundStyle(Palette.secondary)
            Text(entry.data.deliveryBody ?? "").font(.footnote).foregroundStyle(Palette.secondary)
            ForEach(entry.data.deliveryRows ?? [], id: \.deliveryId) { row in
                // 显式重试交给 host 创建新投递，不覆盖旧记录。
                HStack(spacing: 8) {
                    Text("\(model.bots[row.targetBotId]?.name ?? row.targetBotId): \(row.status) \(row.detail)")
                        .font(.footnote).foregroundStyle(Palette.secondary)
                    if ["failed", "interrupted", "noReply"].contains(row.status) {
                        Button("Try again") { model.retryDelivery(row.deliveryId) }.buttonStyle(.secondary)
                    }
                }
            }
        }
    }
}
