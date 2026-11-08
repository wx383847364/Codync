package com.codync.android

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import com.kyant.shapes.RoundedRectangle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Files sent with a message, on the sender's side (iOS `AttachmentList`): images as pictures (tap for full size), other files as cards. */
@Composable internal fun AttachmentList(attachments: List<Attachment>, botId: String, state: AppState, store: AppStore) {
    var viewing by remember { mutableStateOf<Attachment?>(null) }
    Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        for (file in attachments) {
            if (file.isImage) AttachmentImage(file, botId, 720, state, store, Modifier.sizeIn(maxWidth = 220.dp, maxHeight = 280.dp)
                .clip(RoundedRectangle(16.dp)).plainButton { viewing = file }.semantics { contentDescription = file.name })
            else AttachmentCard(file)
        }
    }
    AttachmentImageSheet(viewing, botId, state, store) { viewing = null }
}

/** 查看已发送附件的原始像素，复用带尺寸限制的解码器。 */
@Composable internal fun AttachmentImageSheet(file: Attachment?, botId: String, state: AppState, store: AppStore, dismiss: () -> Unit) {
    ImageSheet(file, { it.name }, dismiss) { shown, modifier -> AttachmentImage(shown, botId, 32768, state, store, modifier) }
}

/** 群聊和私聊分享图片共用原图缩放面板。 */
@Composable internal fun SharedImageSheet(file: SharedFile?, entryId: String, state: AppState, store: AppStore, dismiss: () -> Unit) {
    ImageSheet(file, { it.name }, dismiss) { shown, modifier -> SharedImage(shown, entryId, 32768, state, store, modifier) }
}

/** 复用大图面板容纳已发送图片和本地草稿，内容读取由所属组件负责。 */
@Composable internal fun <T : Any> ImageSheet(item: T?, name: (T) -> String, dismiss: () -> Unit, image: @Composable (T, Modifier) -> Unit) {
    CodyncSheet(item, dismiss) { shown ->
        Column(Modifier.fillMaxSize().background(Palette.background)) {
            ModalHeader(name(shown))
            Box(Modifier.weight(1f).padding(12.dp), contentAlignment = Alignment.Center) { image(shown, Modifier.fillMaxSize()) }
        }
    }
}

@Composable private fun AttachmentCard(file: Attachment) {
    Row(Modifier.clip(RoundedRectangle(16.dp)).background(Palette.bubbleUser).semantics(mergeDescendants = true) { }
        .padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Symbol(AttachmentIcon.symbol(file.name), size = 19.dp, tint = Palette.secondary)
        Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(file.name, style = Type.subheadline, color = Palette.text, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
            Text(formatBytes(file.size), style = Type.caption2, color = Palette.tertiary)
        }
    }
}

/** A sent image, fetched from the computer once and then read from the cache; a placeholder until then. */
@Composable internal fun AttachmentImage(file: Attachment, botId: String, maxPixels: Int, state: AppState, store: AppStore, modifier: Modifier,
    scale: ContentScale = ContentScale.Fit) = RemoteImage(file.id, maxPixels, state, modifier, scale) { store.attachment(botId, file) }

/** An image a bot shared, like `AttachmentImage`. */
@Composable internal fun SharedImage(file: SharedFile, entryId: String, maxPixels: Int, state: AppState, store: AppStore, modifier: Modifier) =
    RemoteImage(file.id, maxPixels, state, modifier, ContentScale.Fit) { store.sharedImage(entryId, file) }

/** 下载并有界解码图片；原图支持手势，失败必须由用户重试或连接恢复。 */
@Composable private fun RemoteImage(id: String, maxPixels: Int, state: AppState, modifier: Modifier, scale: ContentScale, load: suspend () -> java.io.File) {
    var failure by remember(id, maxPixels) { mutableStateOf(false) }
    var retry by remember(id, maxPixels) { mutableStateOf(0) }
    var image by remember(id, maxPixels) { mutableStateOf<ImageBitmap?>(null) }
    // Retried whenever the link changes: an image the computer couldn't send stays a placeholder, as on iOS.
    LaunchedEffect(id, maxPixels, state.actualConnection, retry) {
        if (image != null) return@LaunchedEffect
        failure = false
        try {
            val source = load()
            image = withContext(Dispatchers.IO) { AttachmentFiles.thumbnail(source, maxPixels).asImageBitmap() }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { failure = true; Analytics.capture(AnalyticsEvent.ImageViewed(AnalyticsEvent.ImageOutcome.Failed)) }
    }
    val bitmap = image
    if (failure) Column(modifier) {
        Text("Image preview unavailable", style = Type.caption2, color = Palette.secondary)
        Text("Retry", Modifier.plainButton { /* 只响应明确的重试操作。 */ retry++ }, style = Type.caption2, color = Palette.accent)
    }
    else if (bitmap != null && maxPixels > 4096) ZoomableImage(bitmap, modifier)
    else if (bitmap != null) Image(bitmap, contentDescription = null, modifier = modifier, contentScale = scale)
    else Box(modifier.size(160.dp, 120.dp).clip(RoundedRectangle(16.dp)).background(Palette.bubbleUser), contentAlignment = Alignment.Center) {
        Symbol("photo", size = 20.dp, tint = Palette.tertiary)
    }
}

/** The symbol for a file by its extension. */
internal object AttachmentIcon {
    fun symbol(name: String): String = when (name.substringAfterLast('.', "").lowercase()) {
        "png", "jpg", "jpeg", "heic", "gif", "webp", "tiff", "bmp" -> "photo"
        "pdf" -> "doc.richtext"
        "zip", "gz", "tar", "7z" -> "doc.zipper"
        "mov", "mp4", "m4v" -> "film"
        "mp3", "m4a", "wav", "aac" -> "waveform"
        else -> "doc"
    }
}
