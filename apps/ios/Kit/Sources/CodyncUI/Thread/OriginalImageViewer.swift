import CodyncKit
import ImageIO
import SwiftUI
import UIKit

/// 缩略图之外的原始像素查看器，保留原图细节并限制完整解码的最大像素数。
struct OriginalImageViewer: View {
    let source: AttachmentImage
    @Environment(BotStore.self) private var model
    @State private var image: CGImage?
    @State private var failed = false
    @State private var original = false
    @State private var zoomRequest = UUID()
    @State private var retry = 0

    /// 工具栏与手势均只改变图片显示比例，不修改原始文件。
    var body: some View {
        VStack(spacing: 8) {
            HStack {
                // 图片显示方式使用动画切换，100% 为一个原图像素对应一个逻辑点。
                Button("Fit to window") { Motion.animate { original = false; zoomRequest = UUID() }; Analytics.shared.capture(.imageViewed(.fit)) }
                Button("100%") { Motion.animate { original = true; zoomRequest = UUID() }; Analytics.shared.capture(.imageViewed(.original)) }
            }.buttonStyle(.secondary)
            if let image {
                ZoomableImage(image: image, original: original, request: zoomRequest)
            } else if failed {
                Text("Image preview unavailable").foregroundStyle(Palette.secondary)
                Button("Retry") { /* 只重新读取和解码当前图片。 */ retry += 1 }.buttonStyle(.secondary)
            } else {
                Image(systemName: "photo").foregroundStyle(Palette.tertiary)
            }
        }
        // 原图读取仍交给电脑 store；离开预览后不应用晚到的解码结果。
        .task(id: "\(source.id)-\(retry)") {
            Analytics.shared.capture(.imageViewed(.opened))
            failed = false
            image = nil
            guard let data = await source.load(model) else {
                guard !Task.isCancelled else { return }
                failed = true
                Analytics.shared.capture(.imageViewed(.failed))
                return
            }
            // 原图解码放在后台，取消后的结果不更新已关闭的面板。
            let decoded = await Task.detached { Self.decode(data) }.value
            guard !Task.isCancelled else { return }
            image = decoded
            failed = decoded == nil
            if failed { Analytics.shared.capture(.imageViewed(.failed)) }
        }
    }

    /// 按原始分辨率解码，超过 32 百万像素的附件保留下载能力并明确拒绝预览。
    nonisolated private static func decode(_ data: Data) -> CGImage? {
        guard data.count <= 20 * 1024 * 1024, let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              width > 0, height > 0, width <= 32_768, height <= 32_768,
              Int64(width) * Int64(height) <= 32_000_000 else { return nil }
        return ImageData.thumbnail(data, maxPixels: max(width, height))
    }
}

/// UIKit 原生滚动与缩放承载原图，不把下采样图片拉伸冒充原始细节。
private struct ZoomableImage: UIViewRepresentable {
    let image: CGImage
    let original: Bool
    let request: UUID

    /// 构造原生查看容器，支持双指缩放和双击切换。
    func makeUIView(context: Context) -> ImageScrollView { ImageScrollView(image: image) }

    /// 仅在用户切换显示方式时调整缩放，不打断正在进行的手势。
    func updateUIView(_ view: ImageScrollView, context: Context) { view.show(original: original, request: request) }
}

/// 图片尺寸和滚动区域由 UIKit 的缩放机制统一管理。
private final class ImageScrollView: UIScrollView, UIScrollViewDelegate {
    private let picture: UIImageView
    private var previousSize = CGSize.zero
    private var original = false
    private var previousRequest: UUID?

    /// 固定 UIImage 的 scale 为 1，使逻辑点与原图像素一一对应。
    init(image: CGImage) {
        picture = UIImageView(image: UIImage(cgImage: image, scale: 1, orientation: .up))
        super.init(frame: .zero)
        delegate = self
        addSubview(picture)
        let tap = UITapGestureRecognizer(target: self, action: #selector(toggleZoom))
        tap.numberOfTapsRequired = 2
        addGestureRecognizer(tap)
        isAccessibilityElement = true
        accessibilityTraits = .image
        accessibilityLabel = "Image preview"
    }

    /// 查看器只由代码创建，不支持归档恢复。
    required init?(coder: NSCoder) { return nil }

    /// 窗口改变后重算适应比例，始终保留可查看原始像素的缩放范围。
    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.size != previousSize, bounds.width > 0, bounds.height > 0 else { return }
        previousSize = bounds.size
        let size = picture.image?.size ?? .zero
        guard size.width > 0, size.height > 0 else { return }
        minimumZoomScale = min(1, bounds.width / size.width, bounds.height / size.height)
        maximumZoomScale = max(4, minimumZoomScale * 4)
        setZoomScale(original ? 1 : minimumZoomScale, animated: false)
        contentSize = picture.frame.size
        centerPicture()
    }

    /// 只响应显示方式变化，保持用户拖动的位置。
    func show(original: Bool, request: UUID) {
        // 每次工具栏点击都是独立命令，不能因手势后的布尔值相等而跳过。
        guard previousRequest != request else { return }
        previousRequest = request
        self.original = original
        setZoomScale(original ? 1 : minimumZoomScale, animated: true)
    }

    /// 双击切换适应窗口和原始像素。
    @objc private func toggleZoom() {
        // 根据实际缩放比例决定目标，双指缩放不会让双击失效。
        original = zoomScale < 0.99
        setZoomScale(original ? 1 : minimumZoomScale, animated: true)
    }

    /// 小图或适应窗口状态居中显示，不影响超出视口图片的滚动范围。
    private func centerPicture() {
        let horizontal = max(0, (bounds.width - picture.frame.width) / 2)
        let vertical = max(0, (bounds.height - picture.frame.height) / 2)
        contentInset = UIEdgeInsets(top: vertical, left: horizontal, bottom: vertical, right: horizontal)
    }

    /// 原生手势改变缩放时同步居中，工具栏仍可强制执行查看命令。
    func scrollViewDidZoom(_ scrollView: UIScrollView) { centerPicture() }

    /// 原图视图是唯一可缩放内容。
    func viewForZooming(in scrollView: UIScrollView) -> UIView? { picture }
}
