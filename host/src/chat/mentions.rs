//! 入库时固定提及身份；屏蔽扫描保留原文的字节位置，不依赖显示层 Markdown。

use crate::store::BotConfig;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Mention {
    pub bot_id: String,
    pub start: usize,
    pub end: usize,
}

/// UTF-16 边界转为字节边界；代理对内部不是合法边界。
pub fn byte_offset(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += ch.len_utf16();
    }
    (units == offset).then_some(text.len())
}

/// 在原文校验绑定，再与 host 裁剪同步换算区间；不依赖当前显示名。
pub fn checked(text: &str, mentions: &[Mention], members: &[String]) -> Result<Vec<Mention>> {
    let mut ordered = mentions.to_vec();
    ordered.sort_by_key(|m| m.start);
    let leading = text.len() - text.trim_start().len();
    let shift = text[..leading].encode_utf16().count();
    let trimmed_end = text.trim_end().len();
    let mut previous = 0;
    for m in &mut ordered {
        let start = byte_offset(text, m.start);
        let end = byte_offset(text, m.end);
        ensure!(m.start < m.end && m.start >= previous, "提及范围失效，请重新选择成员");
        let (Some(start), Some(end)) = (start, end) else { bail!("提及范围失效，请重新选择成员") };
        ensure!(
            text[start..end].starts_with('@') && start >= leading && end <= trimmed_end,
            "提及范围失效，请重新选择成员"
        );
        ensure!(members.contains(&m.bot_id), "提及成员已不在群内，请重新选择成员");
        previous = m.end;
        m.start -= shift;
        m.end -= shift;
    }
    Ok(ordered)
}

/// 标记代码、引用、链接和邮箱；只标记字节，不重排或改写原文。
fn masked(text: &str) -> Vec<bool> {
    let bytes = text.as_bytes();
    let mut mask = vec![false; bytes.len()];
    let mut fence: Option<(u8, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let stripped = line.trim_start_matches(' ');
        let spaces = line.len() - stripped.len();
        let marker = stripped.as_bytes().first().copied().unwrap_or(0);
        let count = stripped.bytes().take_while(|b| *b == marker).count();
        if let Some((kind, length)) = fence {
            mask[offset..offset + line.len()].fill(true);
            if spaces <= 3 && marker == kind && count >= length && stripped[count..].trim().is_empty() {
                fence = None;
            }
        } else if spaces <= 3 && matches!(marker, b'`' | b'~') && count >= 3 {
            fence = Some((marker, count));
            mask[offset..offset + line.len()].fill(true);
        } else if spaces >= 4 || line.starts_with('\t') || stripped.starts_with('>') {
            mask[offset..offset + line.len()].fill(true);
        }
        offset += line.len();
    }
    let mut i = 0;
    while i < bytes.len() {
        if mask[i] || bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let length = bytes[i..].iter().take_while(|b| **b == b'`').count();
        let mut j = i + length;
        let mut closing = None;
        while j < bytes.len() {
            if mask[j] {
                break;
            }
            if bytes[j] != b'`' {
                j += 1;
                continue;
            }
            let run = bytes[j..].iter().take_while(|b| **b == b'`').count();
            if run == length {
                closing = Some(j + run);
                break;
            }
            j += run;
        }
        if let Some(end) = closing {
            mask[i..end].fill(true);
            i = end;
        } else {
            i += length;
        }
    }
    // URL 和邮箱按空白分隔的词屏蔽，保留紧邻中文的原始偏移。
    for (start, word) in words(text) {
        let lower = word.to_ascii_lowercase();
        let link = lower.find("http://").or_else(|| lower.find("https://")).or_else(|| lower.find("www."));
        if let Some(at) = link {
            mask[start + at..start + word.len()].fill(true);
        }
        if let Some(at) = word.find('@') {
            let local = &word[..at];
            let domain = &word[at + 1..];
            if local.chars().last().is_some_and(|c| c.is_ascii_alphanumeric() || ".%_+-".contains(c))
                && domain.contains('.')
                && domain.starts_with(|c: char| c.is_ascii_alphanumeric())
            {
                mask[start..start + word.len()].fill(true);
            }
        }
        if word.starts_with('<') && word.ends_with('>') && word.contains(':') {
            mask[start..start + word.len()].fill(true);
        }
    }
    mask
}

/// 按原字节位置遍历非空白词，供链接屏蔽使用。
fn words(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(from) = start.take() {
                out.push((from, &text[from..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(from) = start {
        out.push((from, &text[from..]));
    }
    out
}

/// 匹配名称时使用原始字符串的字符边界，避免 Unicode 小写扩展改变字节偏移。
fn matching_end(text: &str, start: usize, handle: &str) -> Option<usize> {
    let wanted = handle.to_lowercase();
    for end in text[start..].char_indices().map(|(i, ch)| start + i + ch.len_utf8()) {
        let candidate = text[start..end].to_lowercase();
        if candidate == wanted {
            return text[end..].chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_')).then_some(end);
        }
        if candidate.len() > wanted.len() {
            break;
        }
    }
    None
}

/// 结构化片段按 ID 路由，其余正文接受唯一名称；未知 @ 是普通文本。
pub fn targets(text: &str, mentions: &[Mention], members: &[BotConfig], quoted: Option<&str>) -> Result<Vec<String>> {
    let mut mask = masked(text);
    let mut targets: Vec<String> = Vec::new();
    for m in mentions {
        // 成员可能在输入校验后离群，显式提及失效时不能改发给默认接收人。
        ensure!(members.iter().any(|member| member.id == m.bot_id), "提及成员已不在群内，请重新选择成员");
        let range: Range<usize> = byte_offset(text, m.start).ok_or_else(|| anyhow::anyhow!("invalid mention"))?
            ..byte_offset(text, m.end).ok_or_else(|| anyhow::anyhow!("invalid mention"))?;
        mask[range].fill(true);
        if !targets.contains(&m.bot_id) {
            targets.push(m.bot_id.clone());
        }
    }
    for (at, _) in text.match_indices('@') {
        if mask[at] {
            continue;
        }
        if at > 0 && text[..at].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let start = at + 1;
        if matching_end(text, start, "all").is_some() || matching_end(text, start, "everyone").is_some() {
            targets.extend(members.iter().map(|m| m.id.clone()));
            continue;
        }
        let mut exact = Vec::new();
        let mut aliases = Vec::new();
        for member in members {
            let name = member.name.trim();
            if let Some(end) = matching_end(text, start, name).filter(|end| !mask[at..*end].iter().any(|b| *b)) {
                exact.push((end, &member.id));
            }
            for alias in [
                name.split_whitespace().collect::<String>(),
                name.split_whitespace().next().unwrap_or_default().to_owned(),
            ] {
                if !alias.is_empty()
                    && let Some(end) =
                        matching_end(text, start, &alias).filter(|end| !mask[at..*end].iter().any(|b| *b))
                {
                    aliases.push((end, &member.id));
                }
            }
        }
        let candidates = if exact.is_empty() { aliases } else { exact };
        let longest = candidates.iter().map(|(end, _)| *end).max();
        let mut ids: Vec<&String> =
            candidates.into_iter().filter(|(end, _)| Some(*end) == longest).map(|(_, id)| id).collect();
        ids.sort();
        ids.dedup();
        ensure!(ids.len() <= 1, "提及名称有歧义，请重新选择成员");
        if let Some(id) = ids.first() {
            targets.push((*id).clone());
        }
    }
    if let Some(id) = quoted.filter(|id| members.iter().any(|m| m.id == *id)) {
        targets.push(id.to_owned());
    }
    // 保持成员名单顺序，重复文字或结构化提及只形成一条派发。
    Ok(members.iter().filter(|m| targets.contains(&m.id)).map(|m| m.id.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非路由内容内的 @ 不能创建任务，围栏长度和内联反引号必须配对。
    #[test]
    fn code_quotes_links_and_mail_are_not_assignments() {
        let text = "````rust\n```\n@A\n````\n~~~text\n@A\n~~~\n`` @A ` x ``\n    @A\n\t@A\n> @A\n>> @A\n<https://x/@A> www.x/@A a@host.tld";
        let mask = masked(text);
        for (at, _) in text.match_indices('@') {
            assert!(mask[at], "unmasked at {at}");
        }
        assert!(!masked("@all")[0]);
    }

    /// UTF-16 校验拒绝切断代理对，并与正文裁剪同步换算。
    #[test]
    fn unicode_ranges_survive_trim_and_rename() {
        let m = Mention { bot_id: "a".into(), start: 4, end: 7 };
        let normalized = checked("  😀@旧名  ", &[m], &["a".into()]).unwrap();
        assert_eq!(normalized[0].start, 2);
        assert_eq!(normalized[0].end, 5);
        assert_eq!(byte_offset("😀", 1), None);
        assert!(checked("@A", &[Mention { bot_id: "gone".into(), start: 0, end: 2 }], &[]).is_err());
    }
}
