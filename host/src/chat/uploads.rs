//! Files sent with a message. A client uploads each file in chunks (the channel caps a
//! message at 1 MiB), then names the uploads in `send`; the agent gets their paths.

use crate::agent::workspace;
use crate::service;
use crate::store::BotConfig;
use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Inside a personal workspace, so reading an upload needs no extra approval.
/// A group (no workspace) keeps its members' shared files in its data folder.
pub fn root(cfg: &BotConfig) -> PathBuf {
    if workspace::is_managed(cfg) {
        Path::new(&cfg.cwd).join("uploads")
    } else {
        service::data_dir().join("bots").join(&cfg.id).join("uploads")
    }
}

/// `uploads/<upload id>/`, the id checked so it can't leave the folder.
fn dir(root: &Path, upload: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(upload).context("invalid upload id")?;
    Ok(root.join(upload.to_ascii_lowercase()))
}

/// The file's own name only: no folders, no hidden or empty names.
fn clean_name(name: &str) -> Result<String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.is_empty() || name.starts_with('.') || name.len() > 200 {
        bail!("invalid file name");
    }
    Ok(name.to_owned())
}

/// 按偏移追加分片，不限制附件总大小；重试当前分片可重复确认，完成后返回附件元数据。
pub fn append(root: &Path, upload: &str, name: &str, offset: u64, data: &[u8], done: bool) -> Result<Option<Value>> {
    let dir = dir(root, upload)?;
    let path = dir.join(clean_name(name)?);
    std::fs::create_dir_all(&dir).context("creating the upload folder")?;
    let len = std::fs::metadata(&path).map_or(0, |m| m.len());
    // 总大小不设产品上限，但偏移加法必须校验溢出，不能把越界分片误认成重试。
    let end = offset.checked_add(data.len() as u64).context("invalid upload offset")?;
    if offset == len {
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(&path).context("opening the upload")?;
        file.write_all(data).context("writing the upload")?;
    } else if end != len {
        bail!("upload out of order: at {len}, got {offset}");
    }
    Ok(done.then(|| json!({"id": upload, "name": path.file_name().map(|n| n.to_string_lossy()), "size": end})))
}

/// A finished upload: its path and chat metadata.
pub fn resolve(root: &Path, upload: &str) -> Result<(PathBuf, Value)> {
    let dir = dir(root, upload)?;
    let file = std::fs::read_dir(&dir)
        .context("unknown upload")?
        .filter_map(Result::ok)
        .find(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .context("empty upload")?;
    let size = file.metadata().map_or(0, |m| m.len());
    let name = file.file_name().to_string_lossy().into_owned();
    Ok((file.path(), json!({"id": upload, "name": name, "size": size})))
}

/// Where a sent attachment (`{id, name}` chat metadata) lives, without touching the disk.
pub fn path_of(root: &Path, meta: &Value) -> Option<PathBuf> {
    let name = clean_name(meta["name"].as_str()?).ok()?;
    Some(dir(root, meta["id"].as_str()?).ok()?.join(name))
}

/// Up to `len` bytes of a finished upload from `offset`, and its full size.
pub fn read(root: &Path, upload: &str, offset: u64, len: usize) -> Result<(Vec<u8>, u64)> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (path, _) = resolve(root, upload)?;
    let mut file = std::fs::File::open(&path).context("opening the upload")?;
    let size = file.metadata().context("reading the upload")?.len();
    file.seek(SeekFrom::Start(offset)).context("reading the upload")?;
    let mut data = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut data).context("reading the upload")?;
    Ok((data, size))
}

/// Deletes an upload and its folder.
pub fn remove(root: &Path, upload: &str) -> Result<()> {
    std::fs::remove_dir_all(dir(root, upload)?).context("deleting the upload")
}

/// What the agent reads after the message text.
pub fn prompt_suffix(paths: &[PathBuf]) -> String {
    let list: Vec<String> = paths.iter().map(|p| format!("- {}", p.display())).collect();
    format!("\n\n[Attached files — read them from these paths]\n{}", list.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跨过旧 100 MiB 边界继续追加，并拒绝偏移溢出，不在测试中分配整文件内存。
    #[test]
    fn uploads_continue_beyond_the_old_size_limit_and_reject_offset_overflow() {
        let root = std::env::temp_dir().join(format!("codync-large-upload-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        append(&root, &id, "video.mp4", 0, b"", false).expect("prepare");
        let path = dir(&root, &id).expect("dir").join("video.mp4");
        let size = 100 * 1024 * 1024;
        std::fs::OpenOptions::new().write(true).open(&path).expect("open").set_len(size).expect("sparse file");
        let done = append(&root, &id, "video.mp4", size, b"tail", true).expect("large upload").expect("done");
        assert_eq!(done["size"], size + 4);
        assert_eq!(read(&root, &id, size, 4).expect("tail"), (b"tail".to_vec(), size + 4));
        assert!(append(&root, &id, "video.mp4", u64::MAX, b"x", true).is_err());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn removing_an_upload_stays_inside_the_folder() {
        let root = std::env::temp_dir().join(format!("codync-uploads-{}", uuid::Uuid::new_v4()));
        let upload = uuid::Uuid::new_v4().to_string();
        append(&root, &upload, "a.txt", 0, b"hi", true).expect("upload");
        assert!(remove(&root, "../other").is_err());
        remove(&root, &upload).expect("remove");
        assert!(resolve(&root, &upload).is_err());
        std::fs::remove_dir_all(&root).expect("clean up");
    }

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(clean_name("../../etc/passwd").expect("last component"), "passwd");
        assert_eq!(clean_name("C:\\x\\photo.jpg").expect("last component"), "photo.jpg");
        assert!(clean_name("..").is_err());
        assert!(clean_name(".ssh").is_err());
        assert!(clean_name("dir/").is_err());
    }

    #[test]
    fn chunks_append_and_retries_are_idempotent() {
        let root = std::env::temp_dir().join(format!("codync-uploads-{}", uuid::Uuid::new_v4()));
        let root = root.as_path();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(dir(root, "../x").is_err());
        assert!(append(root, &id, "a.txt", 0, b"hello ", false).expect("first").is_none());
        assert!(append(root, &id, "a.txt", 0, b"hello ", false).is_ok(), "retry of the first chunk");
        assert!(append(root, &id, "a.txt", 3, b"xx", false).is_err(), "out of order");
        let done = append(root, &id, "a.txt", 6, b"world", true).expect("last").expect("done");
        assert_eq!(done["size"], 11);
        let (path, meta) = resolve(root, &id).expect("resolve");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "hello world");
        assert_eq!(meta["name"], "a.txt");
        assert_eq!(read(root, &id, 6, 100).expect("read"), (b"world".to_vec(), 11));
        assert!(path.starts_with(root));
        assert_eq!(path_of(root, &done), Some(path));
        let _ = std::fs::remove_dir_all(root);
    }
}
