import CodyncKit
import SwiftUI

/// Every avatar shape in the chosen color, then every color, as wrapping, centered grids
/// (the New bot dialog and the bot's Settings).
struct AvatarPicker: View {
    @Binding var shape: String
    @Binding var color: String

    var body: some View {
        VStack(spacing: 16) {
            WrapLayout(spacing: 6) {
                ForEach(AvatarPalette.shapes, id: \.self) { s in
                    choice("\(s) shape", chosen: s == shape) { shape = s } label: {
                        CharacterAvatar(shape: s, color: color, size: 40)
                    }
                }
            }
            WrapLayout(spacing: 6) {
                ForEach(AvatarPalette.colors) { c in
                    choice(c.label, chosen: c.id == color) { color = c.id } label: {
                        Circle().fill(c.color).frame(width: 28, height: 28)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity)
    }

    private func choice(_ title: String, chosen: Bool, pick: @escaping () -> Void, @ViewBuilder label: () -> some View) -> some View {
        Button { withAnimation(Motion.hover) { pick() } } label: { label().choiceRing(chosen) }
            .buttonStyle(.plain)
            .accessibilityLabel(title)
            .accessibilityAddTraits(chosen ? .isSelected : [])
    }
}

extension View {
    /// The accent ring around a chosen shape, color or group member.
    func choiceRing(_ chosen: Bool) -> some View {
        padding(4).background(Circle().strokeBorder(chosen ? Palette.accent : .clear, lineWidth: 2))
    }
}

/// 等宽选项居中平分各行，混合宽度的状态标签从前缘依次填满。
struct WrapLayout: Layout {
    var spacing: CGFloat
    var leading = false

    /// 前缘布局占满可用宽度，保证测量与放置时的换行边界一致。
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        // SwiftUI 也会用无限宽度探测自然尺寸，返回值仍需有限。
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        let rows = rows(subviews, width: width)
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        let measuredWidth = rows.map(\.width).max() ?? 0
        return CGSize(width: leading ? (width ?? measuredWidth) : measuredWidth, height: height)
    }

    /// 使用测量时的宽度提案放置每个标签，长状态按视图自己的规则截断。
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        let itemProposal = ProposedViewSize(width: leading ? width : nil, height: nil)
        var y = bounds.minY
        for row in rows(subviews, width: leading ? width : bounds.width) {
            var x = leading ? bounds.minX : bounds.midX - row.width / 2
            for i in row.items {
                subviews[i].place(at: CGPoint(x: x, y: y), anchor: .topLeading, proposal: itemProposal)
                x += subviews[i].sizeThatFits(itemProposal).width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row {
        var items: Range<Int>
        var width: CGFloat
        var height: CGFloat
    }

    /// 用每个子视图实际占用的宽高计算各行。
    private func rows(_ subviews: Subviews, width: CGFloat?) -> [Row] {
        let itemProposal = ProposedViewSize(width: leading ? width : nil, height: nil)
        // 混合宽度的标签受整行宽度约束，等宽选项仍使用自然尺寸。
        let sizes = subviews.map { $0.sizeThatFits(itemProposal) }
        return Self.split(sizes.map(\.width), spacing: spacing, width: width ?? .infinity, balanced: !leading).map { items in
            // 同一行以下一行的起点取最高标签为准。
            Row(items: items,
                width: sizes[items].map(\.width).reduce(0, +) + spacing * CGFloat(items.count - 1),
                height: sizes[items].map(\.height).max() ?? 0)
        }
    }

    /// 按可用宽度断行，等宽选项再均分到相同数量的行。
    static func split(_ widths: [CGFloat], spacing: CGFloat, width: CGFloat, balanced: Bool = true) -> [Range<Int>] {
        guard let first = widths.first else { return [] }
        var rows: [Range<Int>] = []
        var start = 0
        var used = first
        for i in widths.indices.dropFirst() {
            if used + spacing + widths[i] > width {
                rows.append(start..<i)
                start = i
                used = widths[i]
            } else {
                used += spacing + widths[i]
            }
        }
        rows.append(start..<widths.count)
        if !balanced { return rows }
        let perRow = (widths.count + rows.count - 1) / rows.count
        // 保持原有等宽选项的均分行为。
        return stride(from: 0, to: widths.count, by: perRow).map { $0..<min($0 + perRow, widths.count) }
    }
}
