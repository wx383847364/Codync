package com.codync.android

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.LocalDensity
import com.codync.android.design.*

/** 在视口中按原始像素绘制，支持双指缩放、拖动与双击切换。 */
@Composable internal fun ZoomableImage(image: ImageBitmap, modifier: Modifier) {
    var viewport by remember { mutableStateOf(IntSize.Zero) }
    var zoom by remember(image) { mutableFloatStateOf(0f) }
    var offset by remember(image) { mutableStateOf(Offset.Zero) }
    // 与桌面 CSS 像素、iOS 点保持一致：100% 时一个原图像素对应一个逻辑 dp。
    val originalScale = LocalDensity.current.density
    val fit = minOf(originalScale, viewport.width.toFloat() / image.width, viewport.height.toFloat() / image.height)
    val shown = if (zoom == 0f) fit else zoom
    LaunchedEffect(image) { /* 仅记录查看行为，不记录图片身份。 */ Analytics.capture(AnalyticsEvent.ImageViewed(AnalyticsEvent.ImageOutcome.Opened)) }
    Column(modifier) {
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            // 原图缩放只改变查看状态，不修改或重新编码文件。
            Text("Fit to window", Modifier.plainButton { zoom = 0f; offset = Offset.Zero; Analytics.capture(AnalyticsEvent.ImageViewed(AnalyticsEvent.ImageOutcome.Fit)) }, color = Palette.accent)
            Text("100%", Modifier.plainButton { zoom = originalScale; offset = Offset.Zero; Analytics.capture(AnalyticsEvent.ImageViewed(AnalyticsEvent.ImageOutcome.Original)) }, color = Palette.accent)
        }
        Canvas(Modifier.weight(1f).fillMaxWidth().semantics { contentDescription = "Image preview" }.onSizeChanged { /* 视口决定适应比例。 */ viewport = it }
            .pointerInput(image, fit) {
                detectTransformGestures { _, pan, scale, _ ->
                    // 限制缩放与拖动范围，始终允许返回适应窗口。
                    zoom = ((if (zoom == 0f) fit else zoom) * scale).coerceIn(fit.coerceAtLeast(0.001f), 4f * originalScale)
                    val x = ((image.width * zoom - viewport.width) / 2).coerceAtLeast(0f)
                    val y = ((image.height * zoom - viewport.height) / 2).coerceAtLeast(0f)
                    offset = Offset((offset.x + pan.x).coerceIn(-x, x), (offset.y + pan.y).coerceIn(-y, y))
                }
            }.pointerInput(image, originalScale) {
                detectTapGestures(onDoubleTap = { /* 双击在原始逻辑像素与适应窗口之间切换。 */ zoom = if (zoom == originalScale) 0f else originalScale; offset = Offset.Zero })
            }) {
            clipRect {
                withTransform({ translate(size.width / 2 + offset.x, size.height / 2 + offset.y); scale(shown, shown, Offset.Zero) }) {
                    drawImage(image, topLeft = Offset(-image.width / 2f, -image.height / 2f))
                }
            }
        }
    }
}
