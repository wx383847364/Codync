import CodyncKit
import SwiftUI

/// The iPhone's Markdown renderer for messages (`MarkdownBlocks`: tables, rules, nested lists
/// too); inline styling comes from `AttributedString(markdown:)`.
/// Equatable so a reply that didn't change skips its body.
public struct MarkdownText: View, Equatable {
    let source: String
    let fileLinks: Bool
    @State private var selectedPath: String?

    /// 只有聊天正文可以把主机路径作为可操作链接。
    public init(_ source: String, fileLinks: Bool = false) {
        self.source = source
        self.fileLinks = fileLinks
    }

    /// 文件菜单是暂态，不参与正文的等值比较。
    nonisolated public static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.source == rhs.source && lhs.fileLinks == rhs.fileLinks
    }

    /// 显示 Markdown，点击文件路径时提供本机剪贴板操作。
    public var body: some View {
        let blocks = MarkdownBlocks.parse(source)
        // 只截短显示文案，复制操作保留完整路径。
        let visiblePath = selectedPath.map { path in path.count > 512 ? String(path.prefix(512)) + "…" : path }
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                // 段落、列表和表格共用同一套链接动作。
                MarkdownBlock(block: block)
            }
        }
        .textSelection(.enabled)
        .environment(\.openURL, OpenURLAction { url in
            // 电脑的路径只显示在菜单里，绝不交给手机的 URL 处理器。
            if let path = Self.filePath(url) {
                guard fileLinks else { return .discarded }
                selectedPath = path
                return .handled
            }
            switch url.scheme?.lowercased() {
            case nil, "file": return .discarded
            default: return .systemAction
            }
        })
        .codyncDialog("File on the host computer", isPresented: Binding(
            get: { /* 只在已选择链接时展示对话框。 */ selectedPath != nil },
            set: { shown in
                // 菜单关闭后释放选中的路径。
                if !shown { selectedPath = nil }
            }
        ), message: visiblePath) {
            // 捕获选中的路径，关闭对话框时仍可复制。
            guard let path = selectedPath else { return [] }
            return [DialogAction("Copy path") {
                // 路径仅进入本机剪贴板，上报中只记录动作。
                Pasteboard.copy(path)
                Analytics.shared.capture(.fileLinkPathCopied)
            }]
        }
    }

    /// 将 URL 还原成 POSIX 或 Windows 的绝对路径，仅接受本机文件 URI。
    static func filePath(_ url: URL) -> String? {
        let target = url.absoluteString
        let fileURL = target.lowercased().hasPrefix("file://")
        var encoded = target
        if fileURL {
            encoded = String(target.dropFirst("file://".count))
            if encoded.lowercased().hasPrefix("localhost/") { encoded = String(encoded.dropFirst("localhost".count)) }
            guard encoded.hasPrefix("/"), !encoded.hasPrefix("//") else { return nil }
        }
        var path = encoded.removingPercentEncoding ?? encoded
        if fileURL && path.range(of: #"^/[A-Za-z]:[/\\]"#, options: .regularExpression) != nil { path.removeFirst() }
        guard path.utf8.count <= 32_768, !path.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }) else { return nil }
        if path.hasPrefix("/"), !path.hasPrefix("//"), !path.hasPrefix("/\\") { return path }
        return path.range(of: #"^[A-Za-z]:[/\\]"#, options: .regularExpression) == nil ? nil : path
    }

    /// Underlines keep links distinct in the monochrome theme; inline code gets a soft tint.
    static func inline(_ s: String) -> AttributedString {
        guard var text = try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))
        else { return AttributedString(s) }
        for run in text.runs where run.link != nil {
            text[run.range].underlineStyle = .single
        }
        for run in text.runs where run.inlinePresentationIntent?.contains(.code) == true {
            text[run.range].backgroundColor = Palette.text.opacity(0.08)
        }
        return text
    }
}

/// One block of a reply.
private struct MarkdownBlock: View {
    let block: MarkdownBlocks.Block

    @ViewBuilder var body: some View {
        switch block {
        case let .paragraph(t):
            Text(MarkdownText.inline(t))
                .font(.body)
                .foregroundStyle(Palette.text)
        case let .heading(t, level):
            Text(MarkdownText.inline(t))
                .font(level == 1 ? .title3.bold() : level == 2 ? .headline : .subheadline.bold())
                .foregroundStyle(Palette.text)
        case let .bullet(t, marker, depth):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(marker).foregroundStyle(Palette.secondary).monospacedDigit()
                Text(MarkdownText.inline(t)).foregroundStyle(Palette.text)
            }
            .font(.body)
            .padding(.leading, CGFloat(depth) * 16)
        case .rule:
            Rectangle().fill(Palette.border).frame(height: 1).padding(.vertical, 4)
        case let .table(header, rows):
            MarkdownTable(header: header, rows: rows)
        case let .quote(t):
            Text(MarkdownText.inline(t))
                .font(.body)
                .foregroundStyle(Palette.secondary)
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Palette.border).frame(width: 3) }
        case let .code(t, _):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(t)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Palette.text)
                    .padding(10)
            }
            .background(Palette.codeBackground, in: RoundedRectangle(cornerRadius: 8))
        }
    }
}

/// A Markdown table: a bold header and hairlines between rows, scrolling sideways when wide.
private struct MarkdownTable: View {
    let header: [String]
    let rows: [[String]]

    var body: some View {
        let columns = max(header.count, rows.map(\.count).max() ?? 0)
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 7) {
                GridRow {
                    ForEach(0 ..< columns, id: \.self) { i in
                        Text(MarkdownText.inline(i < header.count ? header[i] : "")).fontWeight(.semibold)
                    }
                }
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    Rectangle().fill(Palette.border).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                    GridRow {
                        ForEach(0 ..< columns, id: \.self) { i in
                            Text(MarkdownText.inline(i < row.count ? row[i] : ""))
                        }
                    }
                }
            }
            .font(.body)
            .foregroundStyle(Palette.text)
            .fixedSize()
            .padding(.vertical, 2)
        }
    }
}
