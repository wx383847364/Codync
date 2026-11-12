//! Immutable snapshots explicitly published by a bot, independent of its workspace.
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const CHUNK_SIZE: usize = 384 * 1024;
// bot 主动发布快照的既有限额，与用户上传附件的总大小无关。
pub const MAX_FILE: u64 = 100 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SharedFile {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// Removes an unpublished snapshot on every failure, including a dropped actor.
pub struct Snapshot {
    pub meta: SharedFile,
    path: PathBuf,
    committed: bool,
}

impl Snapshot {
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn root(bot: &str) -> PathBuf {
    crate::service::data_dir().join("bots").join(bot).join("files")
}

/// 将分享文件的 UUID 限制在已确认聊天的快照目录。
pub(crate) fn path(root: &Path, id: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).context("invalid file id")?;
    Ok(root.join(id.to_string()))
}

/// Display names never control storage paths; hidden and extensionless files are valid.
pub fn name(value: &str) -> Result<String> {
    ensure!(!value.is_empty() && value.len() <= 200, "file name must be 1–200 bytes");
    ensure!(
        value != "." && value != ".." && !value.contains(['/', '\\', ':']) && !value.chars().any(char::is_control),
        "invalid file name"
    );
    Ok(value.to_owned())
}

pub(crate) fn open_regular(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    ensure!(std::fs::symlink_metadata(path)?.file_type().is_file(), "only regular files can be shared");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).context("opening the file")?;
    ensure!(file.metadata()?.is_file(), "only regular files can be shared");
    Ok(file)
}

/// 在阻塞线程复制稳定的本地文件，复制期间变更的源文件不发布。
pub fn snapshot(
    root: &Path,
    cwd: &Path,
    source: &str,
    display: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Snapshot> {
    let original = cwd.join(source);
    let source = original.canonicalize().context("finding the file")?;
    let display = match display {
        Some(value) => name(value)?,
        None => name(original.file_name().and_then(|v| v.to_str()).context("file name is not UTF-8; provide name")?)?,
    };
    let mut input = open_regular(&source)?;
    let before = input.metadata()?;
    ensure!(before.len() <= MAX_FILE, "file exceeds 100 MiB");
    write_snapshot(root, uuid::Uuid::new_v4(), display, &mut input, cancelled, |input, size| {
        let after = input.metadata()?;
        ensure!(
            size == before.len() && after.len() == before.len() && after.modified()? == before.modified()?,
            "source file changed while sharing; retry"
        );
        Ok(())
    })
}

/// 手工确认的交接以同一不可变快照格式保存，不需要另建临时源文件。
pub(crate) fn snapshot_bytes(root: &Path, display: &str, mut bytes: &[u8]) -> Result<Snapshot> {
    write_snapshot(root, uuid::Uuid::new_v4(), name(display)?, &mut bytes, &AtomicBool::new(false), |_, _| Ok(()))
}

/// 复制已发布的固定文件，保留引用 ID，并核对新群副本的字节和哈希。
pub(crate) fn copy_shared(room: &str, next: &str, meta: &SharedFile) -> Result<Snapshot> {
    let mut input = open_regular(&readable_path(room, meta)?)?;
    let saved = write_snapshot(
        &root(next),
        uuid::Uuid::parse_str(&meta.id)?,
        name(&meta.name)?,
        &mut input,
        &AtomicBool::new(false),
        |_, _| Ok(()),
    )?;
    ensure!(
        saved.meta.sha256 == meta.sha256 && saved.meta.size == meta.size,
        "Evidence file {} no longer matches its saved version.",
        meta.name
    );
    Ok(saved)
}

/// 确认固定文件是本群仍可读取的快照，返回当前实例能实际使用的路径。
pub(crate) fn readable_path(room: &str, meta: &SharedFile) -> Result<PathBuf> {
    let path = path(&root(room), &meta.id)?;
    let file = open_regular(&path).with_context(|| format!("Evidence file {} is unavailable.", meta.name))?;
    ensure!(file.metadata()?.len() == meta.size, "Evidence file {} no longer matches its saved size.", meta.name);
    Ok(path)
}

/// 按分片核对固定证据的实际内容，避免等长修改通过大小检查。
pub(crate) fn verify_shared(room: &str, meta: &SharedFile) -> Result<PathBuf> {
    let path = readable_path(room, meta)?;
    let mut file = open_regular(&path)?;
    let (size, digest) = copy(&mut file, &mut std::io::sink(), &AtomicBool::new(false))?;
    ensure!(
        size == meta.size && digest == meta.sha256,
        "Evidence file {} no longer matches its saved version.",
        meta.name
    );
    Ok(path)
}

/// 共享快照完成后原子改名；上层事务成功前，析构会移除未发布文件。
fn write_snapshot<R: std::io::Read>(
    root: &Path,
    id: uuid::Uuid,
    display: String,
    input: &mut R,
    cancelled: &AtomicBool,
    check: impl FnOnce(&mut R, u64) -> Result<()>,
) -> Result<Snapshot> {
    std::fs::create_dir_all(root).context("creating the shared file folder")?;
    let id = id.to_string();
    let final_path = path(root, &id)?;
    let partial = final_path.with_extension("partial");
    let mut owned = Snapshot {
        meta: SharedFile { id, name: display, size: 0, sha256: String::new() },
        path: partial.clone(),
        committed: false,
    };
    let mut output = OpenOptions::new().write(true).create_new(true).open(&partial)?;
    let (size, digest) = copy(input, &mut output, cancelled)?;
    owned.meta.size = size;
    check(input, size)?;
    output.sync_all()?;
    drop(output);
    ensure!(!cancelled.load(Ordering::Acquire), "file sharing cancelled");
    std::fs::rename(&partial, &final_path)?;
    owned.path = final_path;
    owned.meta.sha256 = digest;
    Ok(owned)
}

fn copy(
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
    cancelled: &AtomicBool,
) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = vec![0; CHUNK_SIZE];
    loop {
        ensure!(!cancelled.load(Ordering::Acquire), "file sharing cancelled");
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(size <= MAX_FILE, "file exceeds 100 MiB");
        output.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
    }
    Ok((size, digest_hex(&hash.finalize())))
}

pub fn digest_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Formatting into a String cannot fail.
        let _ = write!(value, "{byte:02x}");
    }
    value
}

pub fn read(root: &Path, meta: &SharedFile, offset: u64) -> Result<Vec<u8>> {
    ensure!(meta.size <= MAX_FILE && offset <= meta.size, "invalid file offset or size");
    read_chunk(&path(root, &meta.id)?, meta.size, offset, "shared file is missing or damaged")
}

/// 读取完整 Markdown 并验证版本，受已有分片长度和交接正文上限约束。
pub(crate) fn read_markdown(room: &str, meta: &SharedFile) -> Result<String> {
    let data = read(&root(room), meta, 0).context("The handoff file is unavailable.")?;
    ensure!(
        u64::try_from(data.len())? == meta.size && digest_hex(&Sha256::digest(&data)) == meta.sha256,
        "The handoff file no longer matches its saved version."
    );
    Ok(String::from_utf8(data)?)
}

/// Up to `CHUNK_SIZE` bytes of the stored file `path` from `offset` (at most `size`); fails with `damaged` unless the
/// file is `size` bytes long.
pub(crate) fn read_chunk(path: &Path, size: u64, offset: u64, damaged: &str) -> Result<Vec<u8>> {
    let mut file = open_regular(path)?;
    ensure!(file.metadata()?.len() == size, "{damaged}");
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0; usize::try_from((size - offset).min(CHUNK_SIZE as u64))?];
    file.read_exact(&mut data)?;
    Ok(data)
}

/// 只检查已上传或显式分享的图片，读取头部并限制预览资源；超限仍允许下载原文件。
pub fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    let file = open_regular(path)?;
    ensure!(file.metadata()?.len() <= 20 * 1024 * 1024, "image exceeds 20 MiB");
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32_768);
    limits.max_image_height = Some(32_768);
    limits.max_alloc = Some(192 * 1024 * 1024);
    reader.limits(limits);
    let (width, height) = reader.into_dimensions()?;
    ensure!(
        width > 0
            && height > 0
            && width <= 32_768
            && height <= 32_768
            && u64::from(width) * u64::from(height) <= 32_000_000,
        "image exceeds preview dimensions"
    );
    Ok((width, height))
}

#[cfg(test)]
mod tests;
