//! 终端使用消息选择和剪贴板操作处理聊天里的文件链接。

use super::{Entry, Kind};

impl Entry {
    /// 提取回复中的文件链接，保持出现顺序并去重，方便一条消息复制多个目标。
    pub(in crate::tui) fn file_link_paths(&self) -> Vec<String> {
        if self.kind != Kind::Agent {
            return Vec::new();
        }
        let mut paths = Vec::new();
        let mut fence = None;
        for mut line in self.text().lines() {
            // 围栏中的 Markdown 是示例文本，不提供链接操作。
            let trimmed = line.trim_start();
            let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
            if let Some(marker) = marker {
                let count = trimmed.chars().take_while(|&c| c == marker).count();
                if count >= 3 {
                    match fence {
                        Some((open, width))
                            if open == marker && count >= width && trimmed[count..].trim().is_empty() =>
                        {
                            fence = None;
                        }
                        None => fence = Some((marker, count)),
                        _ => {}
                    }
                    continue;
                }
            }
            if fence.is_some() {
                continue;
            }
            while let Some(index) = line.find(['[', '`']) {
                let (prefix, token) = line.split_at(index);
                line = &token[1..];
                if token.starts_with('`') {
                    // 代码片段内的链接只是文字。未闭合的反引号保留为普通字符。
                    let width = token.bytes().take_while(|&c| c == b'`').count();
                    line = &token[width..];
                    if let Some(rest) = after_code_span(line, width) {
                        line = rest;
                    }
                } else if !prefix.ends_with('\\')
                    && let Some(end) = line.find(['[', ']'])
                    && end > 0
                    && let Some(target) = line[end..].strip_prefix("](")
                    && let Some((destination, rest)) = link_destination(target)
                {
                    // 图像标签可消费，但只复制普通链接的目标。
                    line = rest;
                    if !prefix.ends_with('!')
                        && let Some(path) = file_path(destination)
                        && !paths.contains(&path)
                    {
                        paths.push(path);
                    }
                }
            }
        }
        paths
    }
}

/// 返回同样长度的结束反引号之后的文字，更长或更短的反引号属于代码。
fn after_code_span(mut text: &str, width: usize) -> Option<&str> {
    while let Some(index) = text.find('`') {
        let suffix = &text[index..];
        let count = suffix.bytes().take_while(|&byte| byte == b'`').count();
        text = &suffix[count..];
        if count == width {
            return Some(text);
        }
    }
    None
}

/// 读取 Markdown 目标和剩余文字，尖括号形式允许空格。
fn link_destination(target: &str) -> Option<(&str, &str)> {
    if let Some(target) = target.strip_prefix('<') {
        return target.split_once(">)");
    }
    let mut nested = 0;
    for (index, c) in target.char_indices() {
        match c {
            '(' => nested += 1,
            ')' if nested == 0 => return Some((&target[..index], &target[index + 1..])),
            ')' => nested -= 1,
            c if c.is_whitespace() => return None,
            _ => {}
        }
    }
    None
}

/// 只识别原电脑的绝对路径，排除其他 URI、网络共享与控制字符。
fn file_path(target: &str) -> Option<String> {
    let lower = target.to_ascii_lowercase();
    let (encoded, file_url) = if lower.starts_with("file://localhost/") {
        (&target["file://localhost".len()..], true)
    } else if lower.starts_with("file:///") {
        (&target["file://".len()..], true)
    } else {
        (target, false)
    };
    let mut path = match decode_path(encoded) {
        Some(decoded) => decoded,
        None if !file_url => encoded.to_owned(),
        None => return None,
    };
    if file_url && path.starts_with('/') && windows_path(&path[1..]) {
        path.remove(0);
    }
    if path.len() > 32_768 || path.bytes().any(|b| b < 32 || b == 127) {
        return None;
    }
    ((path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\")) || windows_path(&path))
        .then_some(path)
}

/// Windows 的盘符绝对路径也可以在其他系统上复制。
fn windows_path(path: &str) -> bool {
    matches!(path.as_bytes(), [b'a'..=b'z' | b'A'..=b'Z', b':', b'/' | b'\\', ..])
}

/// 对路径做一次百分号解码，保留加号并校验 UTF-8。
fn decode_path(encoded: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut remaining = encoded.as_bytes();
    while let Some((&byte, tail)) = remaining.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            remaining = &tail[2..];
        } else {
            bytes.push(byte);
            remaining = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 只复制真实文件链接，排除示例、图片嵌入、网页和网络文件 URI。
    #[test]
    fn copies_only_file_link_targets_from_chat_replies() {
        let (_, entry) = Entry::parse(&json!({
            "botId": "b", "id": "e", "seq": 1, "kind": "agent",
            "data": {"text": "`[example](/tmp/no)` [`App`](</tmp/My App.app>) [same](/tmp/My%20App.app) [win](file:///C:/work/a%2Bb.png) [remote](file://remote/a) [web](https://example.com) ![inline](/tmp/no.png)\n```\n[demo](/tmp/no)\n```"}
        })).expect("agent entry");
        assert_eq!(entry.file_link_paths(), ["/tmp/My App.app", "C:/work/a+b.png"]);
    }
}
