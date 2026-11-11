use super::Client;
use crate::analytics::reported::{App as Reporter, FileLinkAction, FileLinkOutcome, Reported, Track};
use crate::chat::files::{CHUNK_SIZE, MAX_FILE, SharedFile};
use crate::tui::app::Msg;
use anyhow::{Result, anyhow, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Notify, mpsc::UnboundedSender};

pub struct Download {
    pub id: String,
    cancel: Arc<Notify>,
}
impl Drop for Download {
    fn drop(&mut self) {
        self.cancel.notify_one();
    }
}
impl Download {
    /// Runs `work` (handed this download's id) until it ends or the download is dropped; its outcome is the last
    /// `Msg::FileDownload`.
    pub(super) fn spawn<F>(tx: UnboundedSender<Msg>, work: impl FnOnce(String) -> F) -> Self
    where
        F: Future<Output = Result<String>> + Send + 'static,
    {
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(Notify::new());
        let (work, task) = (work(id.clone()), Self { id: id.clone(), cancel: cancel.clone() });
        tokio::spawn(async move {
            let done = tokio::select! {
                biased;
                () = cancel.notified() => return,
                result = work => result,
            };
            let _ = tx.send(Msg::FileDownload { id, status: done.map_err(|error| format!("{error:#}")), done: true });
        });
        task
    }
}
pub(super) struct Partial(pub(super) PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Clone, Copy)]
pub(in crate::tui) enum SharedFileAction {
    Save,
    Open,
    CopyPath,
    CopyContents,
    Reveal,
}

impl From<SharedFileAction> for FileLinkAction {
    fn from(action: SharedFileAction) -> Self {
        match action {
            SharedFileAction::Save => Self::Save,
            SharedFileAction::Open => Self::Open,
            SharedFileAction::CopyPath => Self::CopyPath,
            SharedFileAction::CopyContents => Self::CopyContents,
            SharedFileAction::Reveal => Self::Reveal,
        }
    }
}

impl Client {
    pub(in crate::tui) fn download_file(
        &self,
        entry: String,
        file: SharedFile,
        action: SharedFileAction,
        tx: UnboundedSender<Msg>,
    ) -> Download {
        let (client, progress) = (self.clone(), tx.clone());
        Download::spawn(tx, move |id| async move {
            let result = apply(&client, &entry, &file, action, &id, &progress).await;
            let outcome = match &result {
                Ok(_) => FileLinkOutcome::Ok,
                Err(error) if error.is::<UnsupportedText>() || error.is::<UnsupportedLaunch>() => {
                    FileLinkOutcome::Unsupported
                }
                Err(_) => FileLinkOutcome::Failed,
            };
            tokio::spawn(async move {
                let body = Track::body(Reporter::Tui, Reported::FileLinkAction { action: action.into(), outcome });
                let _ = client.call("track", &body).await;
            });
            result
        })
    }
}

async fn apply(
    client: &Client,
    entry: &str,
    file: &SharedFile,
    action: SharedFileAction,
    id: &str,
    tx: &UnboundedSender<Msg>,
) -> Result<String> {
    crate::chat::files::name(&file.name)?;
    ensure!(
        file.size <= MAX_FILE && file.sha256.len() == 64 && file.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid file metadata"
    );
    if matches!(action, SharedFileAction::CopyContents) {
        let text = read_text(client, entry, file).await?;
        crate::tui::app::copy(&text);
        return Ok("Copied file contents".into());
    }
    if matches!(action, SharedFileAction::Open) && !can_open(&file.name) {
        return Err(UnsupportedLaunch.into());
    }
    let path = save(client, entry, file, id, tx).await?;
    match action {
        SharedFileAction::Open => {
            ensure!(crate::shell::open(path.as_os_str()), "Could not open the file");
            Ok(format!("Opening {}", path.display()))
        }
        SharedFileAction::CopyPath => {
            crate::tui::app::copy(&path.to_string_lossy());
            Ok(format!("Copied {}", path.display()))
        }
        SharedFileAction::Reveal => {
            ensure!(reveal(&path), "Could not show the file");
            Ok(format!("Saved {}", path.display()))
        }
        _ => Ok(format!("Saved {}", path.display())),
    }
}

fn can_open(name: &str) -> bool {
    let extension = std::path::Path::new(name.trim_end_matches(['.', ' ']))
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    !matches!(extension.as_str(), "app" | "desktop" | "inetloc" | "lnk" | "url" | "webloc")
        && (!cfg!(windows)
            || !matches!(
                extension.as_str(),
                "appref-ms"
                    | "application"
                    | "bat"
                    | "cmd"
                    | "com"
                    | "cpl"
                    | "exe"
                    | "hta"
                    | "inf"
                    | "jar"
                    | "js"
                    | "jse"
                    | "msc"
                    | "msi"
                    | "msp"
                    | "pif"
                    | "ps1"
                    | "reg"
                    | "scf"
                    | "scr"
                    | "sct"
                    | "vb"
                    | "vbe"
                    | "vbs"
                    | "wsf"
                    | "wsh"
            ))
}

#[derive(Debug)]
struct UnsupportedLaunch;

impl std::fmt::Display for UnsupportedLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("This action is unavailable for this file.")
    }
}

impl std::error::Error for UnsupportedLaunch {}

fn reveal(path: &std::path::Path) -> bool {
    let mut command = match std::env::consts::OS {
        "macos" => {
            let mut command = std::process::Command::new("open");
            command.arg("-R").arg(path);
            command
        }
        "windows" => {
            let mut command = std::process::Command::new("explorer");
            command.arg(format!("/select,{}", path.display()));
            command
        }
        _ => return path.parent().is_some_and(|folder| crate::shell::open(folder.as_os_str())),
    };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command.spawn().map(|mut child| std::thread::spawn(move || child.wait())).is_ok()
}

#[derive(Debug)]
struct UnsupportedText;

impl std::fmt::Display for UnsupportedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Copy file contents supports UTF-8 text files up to 1 MiB.")
    }
}

impl std::error::Error for UnsupportedText {}

async fn read_text(client: &Client, entry: &str, file: &SharedFile) -> Result<String> {
    ensure!(file.size <= 1024 * 1024, UnsupportedText);
    let mut bytes = Vec::with_capacity(usize::try_from(file.size)?);
    loop {
        bytes.extend(read_chunk(client, entry, file, bytes.len() as u64).await?);
        if bytes.len() as u64 == file.size {
            break;
        }
    }
    check_digest(&file.sha256, &Sha256::digest(&bytes))?;
    let text = String::from_utf8(bytes).map_err(|_| UnsupportedText)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    ensure!(!text.bytes().any(|byte| matches!(byte, 0..=8 | 11..=12 | 14..=31 | 127)), UnsupportedText);
    Ok(text.to_owned())
}

async fn read_chunk(client: &Client, entry: &str, meta: &SharedFile, offset: u64) -> Result<Vec<u8>> {
    let response = client
        .call("readFile", &json!({"entryId": entry, "fileId": meta.id, "offset": offset}))
        .await
        .map_err(|error| anyhow!(error))?;
    ensure!(response["size"].as_u64() == Some(meta.size), "file size changed");
    decode_chunk(&response, meta.size, offset)
}

fn check_digest(expected: &str, bytes: &[u8]) -> Result<()> {
    ensure!(crate::chat::files::digest_hex(bytes) == expected, "file integrity check failed; press f to retry");
    Ok(())
}

async fn save(client: &Client, entry: &str, meta: &SharedFile, id: &str, tx: &UnboundedSender<Msg>) -> Result<PathBuf> {
    let dir = downloads()?;
    let partial = Partial(dir.join(format!(".codync-{id}.partial")));
    let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&partial.0).await?;
    let mut offset = 0;
    let mut hash = Sha256::new();
    loop {
        let data = read_chunk(client, entry, meta, offset).await?;
        file.write_all(&data).await?;
        hash.update(&data);
        offset += data.len() as u64;
        let _ = tx.send(Msg::FileDownload {
            id: id.to_owned(),
            status: Ok(format!("Downloading {}: {offset}/{} bytes · f cancels", meta.name, meta.size)),
            done: false,
        });
        if offset == meta.size {
            break;
        }
    }
    check_digest(&meta.sha256, &hash.finalize())?;
    file.sync_all().await?;
    drop(file);
    place(&partial, &dir, &meta.name).await
}

/// One `readFile` or `readArtifact` chunk of a `size`-byte file: exactly the bytes from `offset` on.
pub(super) fn decode_chunk(response: &Value, size: u64, offset: u64) -> Result<Vec<u8>> {
    let encoded = response["data"].as_str().ok_or_else(|| anyhow!("invalid chunk"))?;
    ensure!(encoded.len() <= CHUNK_SIZE / 3 * 4, "chunk too large");
    let data = base64::engine::general_purpose::STANDARD.decode(encoded)?;
    ensure!(
        data.len() as u64 == (size - offset).min(CHUNK_SIZE as u64),
        "download truncated or changed; press f to retry"
    );
    Ok(data)
}

pub(super) fn downloads() -> Result<PathBuf> {
    dirs::download_dir().or_else(dirs::home_dir).ok_or_else(|| anyhow!("No Downloads folder"))
}

/// Links a complete file in `dir` as `name` (or `name (2)`…), never replacing one created meanwhile.
pub(super) async fn place(partial: &Partial, dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    for _ in 0..10_000 {
        let path = super::free_path(dir, name);
        match tokio::fs::hard_link(&partial.0, &path).await {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow!("No available filename in Downloads"))
}
