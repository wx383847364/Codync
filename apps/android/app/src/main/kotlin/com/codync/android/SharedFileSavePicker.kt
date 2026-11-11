package com.codync.android

import android.app.Activity
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import com.codync.android.core.Entry
import com.codync.android.core.SharedFile
import java.io.File

/** The thread owns the launcher so scrolling a card away cannot cancel its export. */
@Composable internal fun rememberSharedFileSavePicker(store: AppStore): (Entry, SharedFile) -> Unit {
    var pendingId by rememberSaveable { mutableStateOf<String?>(null) }
    var pendingPath by rememberSaveable { mutableStateOf<String?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val fileId = pendingId
        val source = pendingPath
        pendingId = null
        pendingPath = null
        if (fileId != null && source != null) {
            val target = result.data?.data?.takeIf { result.resultCode == Activity.RESULT_OK }
            store.exportSharedFile(fileId, File(source), target)
        }
    }
    return { entry, file ->
        store.saveFile(entry.id, entry.botId, file, SharedFileAction.SaveAs { source ->
            val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE)
                .setType(AttachmentFiles.mimeType(source)).putExtra(Intent.EXTRA_TITLE, file.name)
            picker.launch(intent)
            pendingId = file.id
            pendingPath = source.path
        })
    }
}
