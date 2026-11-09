# File attachments

Files travel in both directions: users attach inputs, and bots explicitly share generated outputs.

- Composer: the **+** button left of the message box. iPhone: a menu with Photos (sent as JPEG) and
  Files (plus Paste when the clipboard has an image); desktop: an open panel. Files and images can
  also be dropped on the box or pasted: ⌘V / Ctrl+V on the desktop takes a copied image or file
  (`apps/desktop/src/renderer/views/thread/Composer.tsx`); on the iPhone through Paste in the
  + menu.
  HEIC/TIFF/BMP become JPEG (`OutgoingFile.prepared`, `Thread/Attachments.swift`). Picked files show as chips above
  the text and can be removed; a message can be files only. Group chats take files too: they're
  kept in the group's uploads folder and listed to each member under the message. User uploads have no fixed total-size limit; available device memory and disk space still apply.
- Upload: `upload {botId, uploadId, name, offset, data (base64), done}`, 384 KiB per call so each
  stays under the channel's 1 MiB message limit. A repeated chunk is accepted (retry); anything out
  of order is refused. Then `send {…, attachments: [uploadId]}`; the text may be empty.
- Storage (`host/src/chat/uploads.rs`): `<workspace>/uploads/<uploadId>/<name>` for a personal
  workspace (so reading needs no extra approval), otherwise
  `~/.codync/bots/<bot>/uploads/<uploadId>/<name>`. Only UUID ids and plain file names are accepted.
- The agent gets the message text plus an "Attached files" list of absolute paths and reads them with
  its own tools (Claude and Codex read images too). The entry keeps `attachments: [{id, name, size}]`
  for display.
- A queued message taken back (`unqueue`) deletes its uploads: the bot finds nothing of it in its folder, and
  `readUpload` refuses them. A message Stop dropped keeps its files, for the chat's *Not sent* row.
- Files don't go through the relay mailbox: while the computer is offline, a message with files
  waits in the composer's send button (disabled) instead. A failed send keeps its files on the device
  for Resend until the app quits. After that the iPhone and desktop refuse Resend with an error and the
  message stays failed, never sent as text alone (`filesGone`, `filesToResend`); Android saves the files
  with the pending row and the terminal keeps them in its draft.
- Chat: images show as pictures (tap for full size), other files as cards. Other devices fetch a
  sent file with `readUpload {botId, uploadId, offset}` (384 KiB chunks) and cache it; the sender
  caches its own copy at upload.
- Before sending, double-click a desktop image thumbnail (or activate it with the keyboard) to preview it locally; tap it on iPhone and Android. Closing keeps the draft, and Remove remains a separate action. Preview never uploads or sends the image. The terminal has no graphical thumbnails.
- Desktop opens an image in a large panel that fits the window; a large image shrinks to fit
  below the header without cropping, even when the window is resized, and a small one keeps its
  own size. Sent and bot-shared images use the same viewer.
- Terminal UI: dragging a file onto the terminal pastes its path, which becomes an attachment (⌫ on
  an empty draft removes the last one); sent files show as `▤ name size` lines. It uploads with the
  same `upload` chunks.
- A files-only message previews in the roster as its file names.

## Files from a bot

In its own chat, a bot calls the built-in chat MCP tool
`send_file {path, name?}`. Paths may be absolute or relative to its working directory.
The optional name is a display filename without folders. Any regular file type is
accepted, including binary, hidden, extensionless and empty files, up to 100 MiB.
Directories and special files are refused. Group deliveries may publish to their source group; routine and private delegated turns cannot publish files. Artifact cards remain restricted to the bot’s own chat.

The host streams a snapshot into `~/.codync/bots/<chat>/files/<fileId>` and records
its byte size and SHA-256 before publishing a final agent entry with
`files: [{id, name, size, sha256}]`. Copying runs outside the bot actor; Stop,
reconfiguration, timeout and turn completion cancel unpublished copies. A success
response means the file message is already saved. Check the conversation before
retrying an interrupted publication, to avoid duplicate messages.

Snapshots remain available after the original is edited, removed, or its workspace
changes, and after a host restart. Storage follows the host data directory rather
than the bot working directory. Generated files are separate from input uploads;
`attachments` and `readUpload` retain their existing meaning. Older clients show
the entry's filename/size text fallback. No version or compatibility floor changes.

The authenticated `readFile {entryId, fileId, offset}` API requires a nonnegative
integer offset within the saved size and a file belonging to that final agent entry
in an undeleted chat. A group file belongs to the group and stays readable after its author leaves or is deleted. Existing caller permissions apply: paired
clients need Control access. It returns `{data: <base64>, size}` in chunks of at most
384 KiB over loopback, SSH, direct encrypted and relay transports.

Updated clients show a filename, size, file icon and download action. File extension
only selects the icon; it never restricts saving or changes bytes. Desktop uses its
native Save dialog, iPhone hands the verified temporary file to the native share
sheet (including Save to Files), and the terminal's selected-message `f` action saves
to Downloads without replacing existing files. Progress and errors appear alongside
the action; cancel stops further reads, discards late responses and removes partial
files. Retry starts from zero. Account/computer retirement cancels its transfers.
Downloads use bounded memory and check size and SHA-256 before save/export.

An image file (by extension) up to 20 MB also shows inline above its card on iPhone,
Android and desktop, tap/click for full size. For it the client reads the whole file
into memory through the same `readFile` chunks, checks size and SHA-256, and keeps it
in the sent-attachment cache under its file id; a failed read leaves the placeholder.
The terminal UI shows only the card.

This feature adds no previews of other file types, folder packaging, background
transfers, persistent resume or Watch UI. Existing bots do not publish workspace files
automatically: they must call `send_file`.

## Original image viewing and bounds

The default viewer fits the image to the window. **100%** displays original pixels (one image pixel per CSS logical pixel / point / dp); phones support pinch, pan and double-tap. It uses original bytes, not a 4096-pixel thumbnail stretched to size. Local draft preview still does not upload or send.

`hello.fileImageInfo` advertises `imageInfo {entryId, fileId}` or `{botId, uploadId}`. Only existing explicit files and uploads can be inspected, with Control access. It accepts no path. New desktop clients check the header before loading bytes; older hosts keep their existing preview path. Previews are bounded to 20 MiB, 32 megapixels and 32768 pixels per side; a rejected preview does not block downloading the original file.

Desktop byte caches are limited to 64 MiB per computer and two concurrent downloads, with identical requests shared and retirement clearing the cache. Phones queue image reads and use bounded disk caches (512 MiB soft limit, preserving the current file). Failed previews can be retried explicitly.

Group file copying freezes chat and delivery identity. Publication rechecks the started delivery, author and membership under the same lock as Stop and member removal; stale copies cannot publish in a later turn or private chat. Snapshot storage remains owned by the source chat.

## File links in replies

A Markdown link to an absolute POSIX or Windows path, or to a local `file://` URI, refers to the original file on the bot's computer. Use percent encoding or enclose a destination with spaces in angle brackets. An inline code span containing an absolute path also works as a file link, including in existing replies. Percent signs in those paths are literal. Plain paths and top-level fenced code stay as text. Opening a link never publishes a copy to the chat or Library. To deliver a downloadable copy to other devices, the bot calls `send_file`.

- On the bot's computer, the link’s file menu offers **Open**, **Save as…**, **Copy path**, **Copy file contents**, and **Show in Finder** or **Show in file manager**. **Open with…** on macOS offers Cursor and Visual Studio Code when installed in Applications, as well as the default app. System actions also accept source links ending in `:line` or `:line:column` when the complete path does not exist. **Copy path** preserves the link’s destination, including any source location. Images open with the system app. Click a local `.html` or `.htm` link to open it in the default browser, or right-click it for file actions. Copying file contents supports UTF-8 text up to 1 MiB. Folders and app bundles can be opened or revealed.
- On a desktop connected to another computer, iPhone or Android, click or tap the link to copy the original computer's path. In the terminal, select the reply and press `Y` to copy its linked paths, one per line.
- The desktop main process acts when the user activates an HTML link or selects an action in the chat menu. Paths are never sent to analytics. The host API for shared files is unchanged.
