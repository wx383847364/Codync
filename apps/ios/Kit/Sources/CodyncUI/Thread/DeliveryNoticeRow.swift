import CodyncKit
import SwiftUI

/// 折叠时可直接引导单一成员，多位成员的操作在详情中逐一列出。
struct DeliveryNoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false

    /// 仅显示同群中可接收引导的回合，提交时仍由 host 复核。
    private func activeId(_ row: DeliveryRow) -> String? {
        guard row.canGuide == true, row.status == "queued", model.connection == .online,
              let bot = model.bots[row.targetBotId], bot.status == "working", bot.storageError == nil,
              bot.workingChat == entry.botId else { return nil }
        return bot.guidingDeliveryId
    }

    /// 折叠状态属于本地展示，投递状态和操作结果来自 host。
    var body: some View {
        let rows = entry.data.deliveryRows ?? []
        // 绑定渲染时的回合身份，点击迟到时由 host 拒绝旧操作。
        let eligible = rows.compactMap { row in activeId(row).map { (row.deliveryId, $0) } }
        HStack(alignment: .top, spacing: 8) {
            Disclosure(isExpanded: $expanded) {
                Text("第 \(entry.data.deliveryAttempt ?? 1) 次投递" + (entry.data.parentDeliveryId != nil ? " · 重试投递" : ""))
                    .font(.footnote).foregroundStyle(Palette.secondary)
                ForEach(rows, id: \.deliveryId) { row in
                    // 明确选择一个成员；其他成员的投递状态由 host 独立更新。
                    HStack(spacing: 8) {
                        Text("\(model.bots[row.targetBotId]?.name ?? "已移除成员")：\(row.statusText)" + (row.detail.isEmpty ? "" : " · \(row.detail)"))
                            .font(.footnote).foregroundStyle(Palette.secondary)
                        if let id = activeId(row) {
                            Button("引导") { model.guideDelivery(row.deliveryId, activeId: id) }.buttonStyle(.secondary)
                        } else if ["failed", "interrupted", "noReply"].contains(row.status) {
                            Button("重新尝试") { model.retryDelivery(row.deliveryId) }.buttonStyle(.secondary)
                        }
                    }
                }
            } label: {
                Text(DeliveryRow.summary(rows)).font(.footnote).foregroundStyle(Palette.secondary).lineLimit(1)
            }
            if !expanded, let (deliveryId, activeId) = eligible.first {
                Button(eligible.count == 1 ? "引导" : "引导…") {
                    // 多位成员可以接收引导时，先展开列表明确选择目标。
                    if eligible.count == 1 { model.guideDelivery(deliveryId, activeId: activeId) }
                    else { withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { expanded = true } }
                }.buttonStyle(.secondary)
            }
        }
    }
}
