import SwiftUI

/// Text and its selection travel together when the composer replaces a draft.
struct ComposerText {
    var text = ""
    var selection: TextSelection?
    var editRange: Range<Int>?

    /// 光标前的提及片段；非空选区不显示候选，避免覆盖用户选中的正文。
    var mentionRange: Range<String.Index>? {
        let caret: String.Index
        if case .selection(let range) = selection?.indices {
            guard range.isEmpty, range.upperBound <= text.endIndex else { return nil }
            caret = range.lowerBound
        } else { caret = text.endIndex }
        guard let at = text[..<caret].lastIndex(of: "@") else { return nil }
        if at > text.startIndex, !text[text.index(before: at)].isWhitespace { return nil }
        let query = text[text.index(after: at)..<caret]
        guard !query.contains(where: \.isNewline), query.utf16.count <= 24 else { return nil }
        return at..<caret
    }

    /// 查找光标前的斜杠词，路径中的后续斜杠作为搜索词的一部分。
    var workspaceFileRange: Range<String.Index>? {
        let caret: String.Index
        if case .selection(let range) = selection?.indices {
            guard range.isEmpty, range.upperBound <= text.endIndex else { return nil }
            caret = range.lowerBound
        } else { caret = text.endIndex }
        guard caret == text.endIndex || text[caret].isWhitespace else { return nil }
        let start = text[..<caret].lastIndex(where: \.isWhitespace).map { text.index(after: $0) } ?? text.startIndex
        let token = text[start..<caret]
        guard token.hasPrefix("/"), !token.hasPrefix("//"), token.utf16.count <= 256 else { return nil }
        return start..<caret
    }

    /// 只替换当前查询，保留尾文并把光标放回插入内容后。
    mutating func insertMention(_ name: String, replacing replacementRange: Range<String.Index>? = nil) {
        guard let range = replacementRange ?? mentionRange else { return }
        let offset = text.utf16.distance(from: text.startIndex, to: range.lowerBound)
        let token = "@\(name) "
        var replacement = text
        replacement.replaceSubrange(range, with: token)
        replace(with: replacement)
        selection = TextSelection(insertionPoint: String.Index(utf16Offset: offset + token.utf16.count, in: text))
    }

    /// 替换正文时同步丢弃旧选区，程序插入后由调用方设置新光标。
    mutating func replace(with replacement: String) {
        // iOS 18 converts the old selection to UTF-16 offsets during the next field
        // update. Clearing or shortening text without clearing its indices can trap.
        selection = nil
        editRange = nil
        text = replacement
    }

    mutating func insertNewline() {
        guard !text.isEmpty else { return }
        let range = if case .selection(let range) = selection?.indices,
                       range.upperBound <= text.endIndex {
            range
        } else {
            text.endIndex..<text.endIndex
        }
        // A newline after a lone carriage return forms one Character; count scalars.
        let offset = text.unicodeScalars.distance(from: text.startIndex, to: range.lowerBound)
        var replacement = text
        replacement.replaceSubrange(range, with: "\n")
        replace(with: replacement)
        selection = TextSelection(insertionPoint: text.unicodeScalars.index(text.startIndex, offsetBy: offset + 1))
    }
}
