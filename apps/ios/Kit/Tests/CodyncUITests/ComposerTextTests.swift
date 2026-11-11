import CodyncKit
import SwiftUI
import Testing
@testable import CodyncUI

@MainActor @Test(arguments: ["", "1", "中文", "👨‍👩‍👧‍👦", "replacement text"])
func replacingComposerTextDiscardsPreviousSelection(_ replacement: String) {
    var input = ComposerText(text: "previous draft")
    input.selection = TextSelection(insertionPoint: input.text.endIndex)

    input.replace(with: replacement)

    #expect(input.text == replacement)
    #expect(input.selection == nil)
}

@MainActor @Test func composerReturnReplacesSelectionAndMovesCursorAfterNewline() {
    var input = ComposerText(text: "A👨‍👩‍👧‍👦中B")
    let start = input.text.index(after: input.text.startIndex)
    let end = input.text.index(before: input.text.endIndex)
    input.selection = TextSelection(range: start..<end)

    input.insertNewline()

    #expect(input.text == "A\nB")
    guard case .selection(let range) = input.selection?.indices else {
        Issue.record("Expected an insertion point after Return")
        return
    }
    #expect(range.isEmpty)
    #expect(range.lowerBound == input.text.index(input.text.startIndex, offsetBy: 2))
}

@MainActor @Test func composerReturnAfterCarriageReturnKeepsValidCursor() {
    var input = ComposerText(text: "a\r")
    input.insertNewline()
    #expect(input.text == "a\r\n")
    guard case .selection(let range) = input.selection?.indices else {
        Issue.record("Expected a cursor at the end")
        return
    }
    #expect(range.lowerBound == input.text.endIndex)
}

@MainActor @Test func composerReturnAfterSendingDoesNotRestoreOldDraft() {
    var input = ComposerText(text: "1")
    input.selection = TextSelection(insertionPoint: input.text.endIndex)
    input.replace(with: "")
    input.insertNewline()
    #expect(input.text.isEmpty)
    #expect(input.selection == nil)
}

/// 三端按完整 Unicode 标量判断扩展名字；编辑片段后不能继续路由旧身份。
@Test func supplementaryNameEditsReleaseMentionIdentity() {
    let mention = Mention(botId: "a", start: 0, end: 6)
    #expect(Mention.edited(from: "@Alice tail", to: "@Alice𠀀 tail", mentions: [mention]).isEmpty)
    #expect(Mention.edited(from: "@Alice tail", to: "@Alice𠀀 tail", mentions: [mention], replacement: 6..<6).isEmpty)
}

/// 替换中间的斜杠搜索词时保留尾文，中文文件名与光标都可继续编辑。
@MainActor @Test func workspaceReferenceReplacesTheQueryAtTheCaret() {
    var input = ComposerText(text: "😀 /docs/架构 tail")
    input.selection = TextSelection(insertionPoint: String.Index(utf16Offset: 11, in: input.text))
    guard let range = input.workspaceFileRange else {
        Issue.record("Expected the slash query at the caret")
        return
    }
    #expect(input.text[range] == "/docs/架构")
    input.insertMention("架构 说明.md", replacing: range)
    #expect(input.text == "😀 @架构 说明.md  tail")
    input.replace(with: "/foo\n")
    #expect(input.workspaceFileRange == nil)
}
