import CodyncKit
import SwiftUI

/// A list above the box, not a menu (the system-menu rule doesn't apply): hugs its rows and scrolls only once they're
/// taller than 280 points.
private struct ComposerList<Rows: View>: View {
    @ViewBuilder let rows: Rows

    var body: some View {
        CappedHeight(max: 280) {
            ViewThatFits(in: .vertical) {
                stack
                ScrollView { stack }.scrollIndicators(.hidden)
            }
        }
        .glass(in: RoundedRectangle(cornerRadius: 20, style: .continuous))
    }

    private var stack: some View {
        VStack(spacing: 0) { rows }.padding(.vertical, 6)
    }
}

/// A row of a list above the box: its icon, name and what it is; a tap picks it.
private struct ComposerListRow<Icon: View>: View {
    let title: String
    let detail: String
    var warning = false
    let pick: () -> Void
    @ViewBuilder let icon: Icon

    var body: some View {
        Button(action: pick) {
            HStack(spacing: 10) {
                icon.frame(width: 20)
                Text(title)
                    .font(.subheadline)
                    .foregroundStyle(Palette.text)
                    .lineLimit(1)
                    .layoutPriority(1)
                Text(detail)
                    .font(.subheadline)
                    .foregroundStyle(warning ? Palette.warning : Palette.secondary)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressScale())
        .accessibilityLabel(title)
        .accessibilityValue(detail)
    }
}

/// The `/` list: each command's icon, name and what it does; a tap runs it.
struct CommandList: View {
    let commands: [SlashCommand]
    let pick: (SlashCommand) -> Void

    var body: some View {
        ComposerList {
            ForEach(commands) { command in
                ComposerListRow(title: command.title, detail: command.detail, warning: command.warning) {
                    pick(command)
                } icon: {
                    Image(systemName: command.icon)
                        .font(.subheadline)
                        .foregroundStyle(Palette.secondary)
                }
            }
        }
    }
}

/// The `@` list in a group: each member's face, name and description; a tap mentions it.
struct MentionList: View {
    let members: [Bot]
    var selected = 0
    let pick: (Bot) -> Void

    /// 复用统一列表的行与滚动容器，键盘候选保留同名 ID 区分。
    var body: some View {
        ScrollViewReader { proxy in
            ComposerList {
                ForEach(Array(members.enumerated()), id: \.element.id) { index, member in
                    ComposerListRow(title: member.name, detail: "\(member.id.prefix(8)) · \(member.description)") {
                        // 选中稳定成员身份，正文替换由编辑器执行。
                        pick(member)
                    } icon: {
                        CharacterAvatar(bot: member, size: 18, animated: false)
                    }
                    .background(index == selected ? Palette.surface : .clear, in: RoundedRectangle(cornerRadius: 10))
                    .id(index)
                }
            }
            // 循环高亮时滚入可见区，输入焦点仍留在编辑器。
            .onChange(of: selected) { _, index in withAnimation(Motion.layout) { proxy.scrollTo(index) } }
        }
    }
}
