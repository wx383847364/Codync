import CodyncKit
import SwiftUI
import UniformTypeIdentifiers

/// 待发送附件：点击图片打开本地大图，移除按钮与预览独立，不上传也不发送。
struct ComposerAttachment: View {
    let file: OutgoingFile
    let remove: () -> Void
    @State private var image: CGImage?
    @State private var loaded = false
    @State private var viewing = false
    @Environment(\.displayScale) private var displayScale

    private var size: CGFloat { 112 }
    private var expectsImage: Bool {
        UTType(filenameExtension: (file.name as NSString).pathExtension)?.conforms(to: .image) == true
    }

    /// 图片预览和移除保持独立，解码失败时退回可移除的文件名。
    var body: some View {
        Group {
            if image != nil || (expectsImage && !loaded) {
                ZStack {
                    RoundedRectangle(cornerRadius: 16).fill(Palette.surface)
                    if let image {
                        Image(decorative: image, scale: displayScale)
                            .resizable()
                            .scaledToFit()
                    } else {
                        Image(systemName: "photo").foregroundStyle(Palette.secondary)
                    }
                }
                .frame(width: size, height: size)
                .clipShape(RoundedRectangle(cornerRadius: 16))
                .accessibilityLabel("Image attachment: \(file.name)")
                .accessibilityAddTraits(.isButton)
                // 触摸端沿用已发送图片的单击预览；移除按钮位于独立覆盖层。
                .onTapGesture {
                    Analytics.shared.capture(.attachmentPreview(.opened))
                    withAnimation(Motion.layout) { viewing = true }
                }
                .overlay(alignment: .topTrailing) {
                    removeButton
                        .foregroundStyle(.white)
                        .background(.black.opacity(0.75), in: Circle())
                        .padding(4)
                }
            } else {
                HStack(spacing: 6) {
                    Image(systemName: AttachmentIcon.symbol(file.name))
                        .foregroundStyle(Palette.secondary)
                    Text(file.name)
                        .font(.footnote)
                        .foregroundStyle(Palette.text)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: 160, alignment: .leading)
                    removeButton.foregroundStyle(Palette.secondary)
                }
                .padding(.leading, 10)
                .padding(.trailing, 4)
                .padding(.vertical, 4)
                .background(Palette.surface, in: Capsule())
            }
        }
        // 关闭面板后保留原附件与输入框正文。
        .codyncSheet(isPresented: $viewing) { AttachmentViewer(local: file) }
        .task(id: file.id) {
            let data = file.data
            let pixels = Int(size * displayScale)
            let thumbnail = await Task.detached(priority: .userInitiated) {
                ImageData.thumbnail(data, maxPixels: pixels)
            }.value
            guard !Task.isCancelled else { return }
            image = thumbnail
            loaded = true
            // 只上报解码失败类别，不上传文件名或图片内容。
            if thumbnail == nil && expectsImage { Analytics.shared.capture(.attachmentPreview(.decodeFailed)) }
        }
    }

    private var removeButton: some View {
        Button(action: remove) {
            Image(systemName: "xmark")
                .font(.system(size: 11, weight: .bold))
                .frame(width: 28, height: 28)
                .contentShape(Circle())
        }
        .buttonStyle(PressScale())
        .accessibilityLabel("Remove \(file.name)")
        .help("Remove")
    }
}
