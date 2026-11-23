//! Persistent, automatically allocated bot workspaces. A cwd is an execution
//! default, not an access boundary; explicit project folders remain opt-in.

use crate::store::BotConfig;
use crate::{LockExt, service};
use anyhow::{Context as _, Result};
use std::collections::HashMap;
#[cfg(unix)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub fn path(id: &str) -> PathBuf {
    service::data_dir().join("bots").join(id).join("workspace")
}

/// 判断执行目录是否正使用 Codync 为该 Bot 分配的托管路径。
pub fn is_managed(cfg: &BotConfig) -> bool {
    !cfg.is_group() && Path::new(&cfg.cwd) == path(&cfg.id)
}

/// Called on a blocking thread before persisting a bot configuration.
/// Empty cwd opts into automatic allocation, including when editing a bot.
pub fn prepare(cfg: &mut BotConfig) -> Result<()> {
    if cfg.is_group() || !cfg.cwd.is_empty() {
        return Ok(());
    }
    // Only host-issued IDs may become directory components.
    uuid::Uuid::parse_str(&cfg.id).context("invalid bot workspace id")?;
    let dir = path(&cfg.id);
    std::fs::create_dir_all(&dir).context("creating bot workspace")?;
    cfg.cwd = dir.to_string_lossy().into_owned();
    Ok(())
}

type Storage = (Option<u64>, bool);
static STORAGE_CACHE: LazyLock<Mutex<HashMap<PathBuf, (Instant, Storage)>>> = LazyLock::new(Mutex::default);

/// 只统计本机托管工作区，旧工作区在改用外部项目后仍计入。最近的扫描跨群复用。
pub(crate) fn storage(id: &str, deadline: Instant) -> Storage {
    if uuid::Uuid::parse_str(id).is_err() {
        return (None, false);
    }
    let root = path(id);
    let parent = service::data_dir();
    // 只允许真实托管目录进入扫描和 Finder，父目录或工作区自身的链接也不能改变归属。
    let resolved = match root.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Some(0), false),
        Err(_) => return (None, false),
    };
    if !parent.canonicalize().is_ok_and(|base| resolved == base.join("bots").join(id).join("workspace"))
        || !std::fs::symlink_metadata(&root).is_ok_and(|meta| meta.is_dir())
    {
        return (None, false);
    }
    let now = Instant::now();
    {
        let mut cache = STORAGE_CACHE.locked();
        cache.retain(|_, (at, _)| now.saturating_duration_since(*at) < Duration::from_secs(30));
        if let Some((_, value)) = cache.get(&root) {
            return *value;
        }
    }
    let result = (allocated_bytes(&resolved, deadline).ok(), true);
    if result.0.is_some() {
        STORAGE_CACHE.locked().insert(root, (now, result));
    }
    result
}

/// 用分配块读数统计 Unix 占用，并对硬链接去重；Windows 使用文件长度。
fn allocated_bytes(root: &Path, deadline: Instant) -> Result<u64> {
    let mut bytes = 0u64;
    let mut dirs = vec![std::fs::read_dir(root)?];
    #[cfg(unix)]
    let mut linked = HashSet::new();
    while let Some(dir) = dirs.last_mut() {
        anyhow::ensure!(Instant::now() < deadline, "workspace scan timed out");
        let Some(entry) = dir.next() else {
            dirs.pop();
            continue;
        };
        // 取 DirEntry 的元数据不跟随符号链接；扫描过程中被移走的文件由下次读数覆盖。
        let entry = entry?;
        let item = (|| -> std::io::Result<()> {
            let meta = entry.metadata()?;
            if meta.is_dir() {
                dirs.push(std::fs::read_dir(entry.path())?);
            } else if meta.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if meta.nlink() == 1 || linked.insert((meta.dev(), meta.ino())) {
                        bytes = bytes.saturating_add(meta.blocks().saturating_mul(512));
                    }
                }
                #[cfg(not(unix))]
                {
                    bytes = bytes.saturating_add(meta.len());
                }
            }
            Ok(())
        })();
        if let Err(error) = item
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};

    /// 同一文件即使有两个名称只计一次，指向外部目录的链接不计入。
    #[test]
    fn occupied_bytes_count_hard_links_once_and_skip_symlinks() {
        let temp = std::env::temp_dir().join(format!("codync-storage-{}", uuid::Uuid::new_v4()));
        let root = temp.join("workspace");
        let outside = temp.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let file = root.join("report");
        fs::write(&file, vec![1u8; 32 * 1024]).unwrap();
        fs::hard_link(&file, root.join("copy")).unwrap();
        fs::write(outside.join("model"), vec![2u8; 128 * 1024]).unwrap();
        symlink(&outside, root.join("linked-project")).unwrap();
        let expected = fs::metadata(&file).unwrap().blocks() * 512;
        let actual = allocated_bytes(&root, Instant::now() + Duration::from_secs(1)).unwrap();
        fs::remove_dir_all(temp).unwrap();
        assert_eq!(actual, expected);
    }
}
