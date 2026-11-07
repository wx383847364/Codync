import CodyncKit
import SwiftUI

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
