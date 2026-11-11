import SwiftUI
import UIKit

/// 使用原生输入法状态协调候选键盘，组合文字期间不抢走确认键或改写选区。
struct MentionComposerField: UIViewRepresentable {
    @Binding var input: ComposerText
    var focused: FocusState<Bool>.Binding
    let placeholder: String
    let menuOpen: Bool
    var selectsFileOnReturn = false
    let key: (String) -> Void

    /// 创建原生多行输入，尺寸仍由 Composer 的字体和外边距控制。
    func makeUIView(context: Context) -> Field {
        let view = Field()
        view.backgroundColor = .clear
        view.font = .preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.delegate = context.coordinator
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.accessibilityLabel = placeholder
        return view
    }

    /// 仅同步真实的程序编辑；输入法自己的文字和选区不通过回写打断。
    func updateUIView(_ view: Field, context: Context) {
        context.coordinator.parent = self
        view.menuOpen = menuOpen
        view.selectsFileOnReturn = selectsFileOnReturn
        view.menuKey = key
        view.accessibilityLabel = placeholder
        if view.markedTextRange == nil {
            let returnKey: UIReturnKeyType = selectsFileOnReturn ? .done : .default
            if view.returnKeyType != returnKey {
                view.returnKeyType = returnKey
                if view.isFirstResponder { view.reloadInputViews() }
            }
            if view.text != input.text { view.text = input.text }
            if case .selection(let range) = input.selection?.indices,
               range.upperBound <= input.text.endIndex {
                let selected = NSRange(range, in: input.text)
                if view.selectedRange != selected { view.selectedRange = selected }
            }
        }
        if focused.wrappedValue, !view.isFirstResponder { view.becomeFirstResponder() }
    }

    /// 随文字增高到八行，超过时只滚动输入框内部。
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: Field, context: Context) -> CGSize? {
        let width = proposal.width ?? 240
        let height = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        let line = uiView.font?.lineHeight ?? 22
        return CGSize(width: width, height: min(max(line, height), line * 8))
    }

    /// delegate 持有最新绑定，防止重绘后向旧草稿写入。
    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: MentionComposerField

        /// 保存当前输入目标与绑定。
        init(_ parent: MentionComposerField) { self.parent = parent }

        /// UIKit 给出的原选区是 UTF-16 范围，不能用字符串差异猜测粘贴位置。
        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            // 软件键盘的确认与硬件回车共用选择；输入法仍拥有未提交文字。
            if text == "\n", parent.selectsFileOnReturn, textView.markedTextRange == nil {
                parent.key("\r")
                return false
            }
            parent.input.editRange = range.location..<(range.location + range.length)
            return true
        }

        /// 文本与选区一起回写；marked text 由 UIKit 持有，SwiftUI 不重设它。
        func textViewDidChange(_ textView: UITextView) {
            parent.input.text = textView.text
            textViewDidChangeSelection(textView)
            textView.invalidateIntrinsicContentSize()
        }

        /// 光标移动也会刷新当前 @ 查询，非空选区不显示候选。
        func textViewDidChangeSelection(_ textView: UITextView) {
            guard textView.markedTextRange == nil,
                  let range = Range(textView.selectedRange, in: parent.input.text) else { return }
            parent.input.selection = TextSelection(range: range)
        }

        /// 用户触摸输入框后保持 Composer 的焦点状态一致。
        func textViewDidBeginEditing(_ textView: UITextView) { parent.focused.wrappedValue = true }

        /// 系统收起键盘时清除焦点标记。
        func textViewDidEndEditing(_ textView: UITextView) { parent.focused.wrappedValue = false }
    }

    final class Field: UITextView {
        var menuOpen = false
        var selectsFileOnReturn = false
        var menuKey: ((String) -> Void)?

        /// 仅候选打开且没有组合文字时提供优先按键；Shift+Enter 继续换行。
        override var keyCommands: [UIKeyCommand]? {
            guard menuOpen, markedTextRange == nil else { return super.keyCommands }
            let inputs = [UIKeyCommand.inputUpArrow, UIKeyCommand.inputDownArrow, "\r", "\t", UIKeyCommand.inputEscape]
            var commands = (super.keyCommands ?? []) + inputs.map { input in
                let command = UIKeyCommand(input: input, modifierFlags: [], action: #selector(pickMention(_:)))
                command.wantsPriorityOverSystemBehavior = true
                return command
            }
            if selectsFileOnReturn {
                let newline = UIKeyCommand(input: "\r", modifierFlags: .shift, action: #selector(insertLine))
                newline.wantsPriorityOverSystemBehavior = true
                commands.append(newline)
            }
            return commands
        }

        /// 硬件 Shift+Enter 保留换行，避开软键盘的文件确认。
        @objc private func insertLine() { menuKey?("shift-return") }

        /// 确认后立即关闭原生拦截，重复确认不会重复插入或发送消息。
        @objc private func pickMention(_ command: UIKeyCommand) {
            guard menuOpen, markedTextRange == nil, let input = command.input else { return }
            if ["\r", "\t", UIKeyCommand.inputEscape].contains(input) { menuOpen = false }
            menuKey?(input)
        }
    }
}
