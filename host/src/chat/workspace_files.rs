//! 搜索当前会话的工作区，并在发送时核验选择的文件身份。

use super::mentions::{Mention, byte_offset};
use crate::store::{BotConfig, Store};
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Reference {
    pub path: String,
    pub workspace: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub name: String,
    pub source: String,
    pub file: Reference,
}

const RESULT_LIMIT: usize = 16;
const MAX_REFERENCES: usize = 20;
const CHOOSE_AGAIN: &str = "A referenced workspace file is unavailable. Choose it again with /.";

/// 群会话只能搜索当前成员的工作区，按群成员次序保留同一目录的第一个来源。
pub fn sources(store: &Store, chat: &BotConfig) -> Result<Vec<BotConfig>> {
    if !chat.is_group() {
        return Ok(vec![chat.clone()]);
    }
    let mut sources = Vec::new();
    for id in &chat.members {
        if let Some(row) = store.bot(id)?.filter(|row| !row.deleted && !row.config.is_group()) {
            sources.push(row.config);
        }
    }
    Ok(sources)
}

/// 用规范目录的摘要区分同一 Bot 换工作目录前后的文件选择。
fn root(bot: &BotConfig) -> Result<(PathBuf, String)> {
    let cwd = Path::new(&bot.cwd);
    ensure!(cwd.is_absolute(), CHOOSE_AGAIN);
    let path = dunce::canonicalize(cwd).context(CHOOSE_AGAIN)?;
    ensure!(path.is_dir(), CHOOSE_AGAIN);
    let workspace = super::files::digest_hex(path.as_os_str().as_encoded_bytes());
    Ok((path, workspace))
}

/// 依据文件名优先排序，返回有限候选；忽略隐藏、Git 忽略项和符号链接。
pub fn search(bots: &[BotConfig], query: &str) -> Result<Vec<Candidate>> {
    ensure!(query.len() <= 1024 && !query.chars().any(char::is_control), "Invalid file search.");
    let query = query.replace('\\', "/").to_lowercase();
    let mut seen = HashSet::new();
    let roots = bots
        .iter()
        .filter_map(|bot| {
            let (root, workspace) = root(bot).ok()?;
            seen.insert(root.clone()).then_some((bot, root, workspace))
        })
        .collect::<Vec<_>>();
    let mut ranked = Vec::new();
    // 大型工作区只能使用自己的扫描额度，群内其余来源仍可给出结果。
    let per_root = 60_000 / roots.len().max(1);
    let per_root_time = Duration::from_millis(450) / u32::try_from(roots.len().max(1))?;
    for (bot, root, workspace) in roots {
        let deadline = Instant::now() + per_root_time;
        let walker = ignore::WalkBuilder::new(&root).require_git(false).build();
        for entry in walker.take(per_root) {
            if Instant::now() >= deadline {
                break;
            }
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&root) else { continue };
            let Some(path) = relative.to_str().map(|p| p.replace(std::path::MAIN_SEPARATOR, "/")) else { continue };
            let Some(name) = file_name(&path) else { continue };
            let lower_name = name.to_lowercase();
            let score = if lower_name == query {
                0
            } else if lower_name.starts_with(&query) {
                1
            } else if lower_name.contains(&query) {
                2
            } else if path.to_lowercase().contains(&query) {
                3
            } else {
                continue;
            };
            ranked.push((
                score,
                Candidate {
                    name: name.to_owned(),
                    source: if bots.len() == 1 { String::new() } else { bot.name.clone() },
                    file: Reference { path, workspace: workspace.clone() },
                },
            ));
        }
    }
    // 同分按相对路径排序，避免相同查询时高亮行跳动。
    ranked.sort_unstable_by(|(a, x), (b, y)| {
        a.cmp(b).then_with(|| x.file.path.cmp(&y.file.path)).then_with(|| x.file.workspace.cmp(&y.file.workspace))
    });
    Ok(ranked.into_iter().take(RESULT_LIMIT).map(|(_, candidate)| candidate).collect())
}

/// 只接受可用单行标签展示的工作区相对路径。
fn file_name(path: &str) -> Option<&str> {
    if path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| part.is_empty() || part.starts_with('.') || part.contains(':'))
    {
        return None;
    }
    let name = Path::new(path).file_name()?.to_str()?;
    (name == name.trim()).then_some(name)
}

/// 入库前校验文件名、工作区身份及实际路径，阻止越界、失效文件和切换目录后误读同名文件。
pub fn resolve(bots: &[BotConfig], text: &str, mentions: &[Mention]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for mention in mentions.iter().filter(|mention| mention.workspace_file.is_some()) {
        ensure!(paths.len() < MAX_REFERENCES, "Choose at most 20 workspace files for one message.");
        let file = mention.workspace_file.as_ref().context(CHOOSE_AGAIN)?;
        let name = file_name(&file.path).context(CHOOSE_AGAIN)?;
        let start = byte_offset(text, mention.start).context(CHOOSE_AGAIN)?;
        let end = byte_offset(text, mention.end).context(CHOOSE_AGAIN)?;
        ensure!(text.get(start..end).and_then(|label| label.strip_prefix('@')) == Some(name), CHOOSE_AGAIN);
        let (root, _) = bots
            .iter()
            .filter_map(|bot| root(bot).ok())
            .find(|(_, workspace)| *workspace == file.workspace)
            .context(CHOOSE_AGAIN)?;
        let relative = Path::new(&file.path);
        ensure!(relative.components().all(|part| matches!(part, Component::Normal(_))), CHOOSE_AGAIN);
        let requested = root.join(relative);
        let path = dunce::canonicalize(&requested).context(CHOOSE_AGAIN)?;
        // 搜索不跟随符号链接；发送同样拒绝后来换成链接的结果。
        ensure!(path == requested && path.is_file(), CHOOSE_AGAIN);
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同名结果保留来源路径，忽略规则及隐藏文件不会暴露。
    #[test]
    fn finds_exact_names_and_paths_with_ignores() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        for name in ["docs/架构 说明.md", "other/架构 说明.md", "ignored/架构 说明.md", ".env", ".gitignore"]
        {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "ignored/\n").unwrap();
        }
        let bot: BotConfig = serde_json::from_value(serde_json::json!({
            "id": "bot", "name": "Bot", "cwd": dir, "permission": "auto"
        }))
        .unwrap();
        let hits = search(std::slice::from_ref(&bot), "架构").unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.file.path.as_str()).collect::<Vec<_>>(),
            ["docs/架构 说明.md", "other/架构 说明.md"]
        );
        let hit = &hits[1];
        let text = "😀 @架构 说明.md";
        let mention = Mention {
            bot_id: None,
            start: 3,
            end: text.encode_utf16().count(),
            workspace_file: Some(hit.file.clone()),
        };
        let bots = std::slice::from_ref(&bot);
        assert_eq!(
            resolve(bots, text, std::slice::from_ref(&mention)).unwrap(),
            [dunce::canonicalize(dir.join(&hit.file.path)).unwrap()]
        );
        std::fs::remove_file(dir.join(&hit.file.path)).unwrap();
        assert!(resolve(bots, text, &[mention]).is_err());
        assert!(search(bots, ".env").unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
