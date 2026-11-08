import Testing
@testable import CodyncKit

/// 投递状态摘要只描述当前结果；混合状态不能误报所有成员都已停止。
@Test func deliverySummariesUseChineseStatusesWithoutRepeatingMessages() {
    let stopped = DeliveryRow(deliveryId: "d", targetBotId: "b", status: "cancelled", detail: "对话轮次上限")
    #expect(DeliveryRow.summary([stopped]) == "自动接力已达 3 轮，已停止继续派工")
    let running = DeliveryRow(deliveryId: "r", targetBotId: "c", status: "started", detail: "")
    #expect(DeliveryRow.summary([stopped, running]) == "群内派工：1 项已停止，1 项执行中")
    #expect(DeliveryRow.summary([]) == "群内派工状态待更新")
    #expect(DeliveryRow(deliveryId: "x", targetBotId: "b", status: "future", detail: "").statusText == "状态待更新")
    let states = ["queued", "claimed", "started", "completed", "failed", "interrupted", "noReply", "cancelled"]
    #expect(states.map { DeliveryRow(deliveryId: "s", targetBotId: "b", status: $0, detail: "").statusText } ==
            ["等待执行", "准备执行", "执行中", "已完成", "失败", "已中断", "未回复", "已停止"])
}

@Test func rosterPreviewsReadFormattedRepliesWithoutMarkdownMarkers() {
    #expect(MessagePreview.text("算好了：**123 × 45 = 5,535** ✅\nDetails") == "算好了：123 × 45 = 5,535 ✅")
    #expect(MessagePreview.text("\n  > **Ready**: `my_file.ts`\r\nMore") == "Ready: my_file.ts")
    #expect(MessagePreview.text("### Release notes") == "Release notes")
    #expect(MessagePreview.text("  \n\t") == "")
    #expect(MessagePreview.text("2 * 3 = 6; my_file.ts") == "2 * 3 = 6; my_file.ts")
}
