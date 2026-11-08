//! The text field every composer and sheet types into.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A one-line-or-more text field with readline-style editing.
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct Editor {
    pub text: String,
    /// Byte offset, always on a char boundary.
    pub cursor: usize,
    #[serde(default)]
    pub mentions: Vec<crate::chat::mentions::Mention>,
}

impl Editor {
    /// 从正文创建编辑器，旧文本默认没有结构化身份。
    pub fn with(text: &str) -> Self {
        Self { text: text.to_owned(), cursor: text.len(), mentions: vec![] }
    }

    /// 清空正文与身份，不能让旧提及范围附着新草稿。
    pub fn clear(&mut self) {
        self.mentions.clear();
        self.text.clear();
        self.cursor = 0;
    }

    /// 插入正文并维护结构化身份，片段末尾扩展名字时解除原身份。
    pub fn insert(&mut self, s: &str) {
        self.move_bindings(self.cursor, self.cursor, s);
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// 一次替换同时维护删除与插入的 UTF-16 差值，保留后方既有身份。
    pub fn replace_range(&mut self, from: usize, to: usize, text: &str) {
        self.move_bindings(from, to, text);
        self.text.replace_range(from..to, text);
        self.cursor = from + text.len();
    }

    /// 编辑片段外平移 UTF-16 范围，编辑内部则解除绑定。
    fn move_bindings(&mut self, from: usize, to: usize, text: &str) {
        let inserted = text.encode_utf16().count();
        let extends_name = text.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_');
        let start = self.text[..from].encode_utf16().count();
        let end = self.text[..to].encode_utf16().count();
        // 插入名字字符会改变可见 token；标点、空格与外部编辑保留稳定身份。
        self.mentions.retain_mut(|m| {
            if m.end <= start {
                return !(m.end == start && inserted > 0 && extends_name);
            }
            if m.start >= end {
                m.start = m.start + inserted - (end - start);
                m.end = m.end + inserted - (end - start);
                return true;
            }
            false
        });
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
    }

    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.text[..i].ends_with(char::is_whitespace) {
            i = self.prev(i);
        }
        while i > 0 && !self.text[..i].ends_with(char::is_whitespace) {
            i = self.prev(i);
        }
        i
    }

    fn word_right(&self) -> usize {
        let mut i = self.cursor;
        while i < self.text.len() && self.text[i..].starts_with(char::is_whitespace) {
            i = self.next(i);
        }
        while i < self.text.len() && !self.text[i..].starts_with(char::is_whitespace) {
            i = self.next(i);
        }
        i
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i)
    }

    /// Editing keys shared by every text field. Returns whether the key was used.
    pub fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('b') if ctrl => self.cursor = self.prev(self.cursor),
            KeyCode::Char('f') if ctrl => self.cursor = self.next(self.cursor),
            KeyCode::Char('b') if alt => self.cursor = self.word_left(),
            KeyCode::Char('f') if alt => self.cursor = self.word_right(),
            KeyCode::Char('w') if ctrl => self.cut(self.word_left(), self.cursor),
            KeyCode::Backspace if alt || ctrl => self.cut(self.word_left(), self.cursor),
            KeyCode::Char('d') if alt => self.cut(self.cursor, self.word_right()),
            KeyCode::Char('u') if ctrl => self.cut(self.line_start(), self.cursor),
            KeyCode::Char('h') if ctrl => self.cut(self.prev(self.cursor), self.cursor),
            KeyCode::Char('d') if ctrl => self.cut(self.cursor, self.next(self.cursor)),
            KeyCode::Left if alt || ctrl => self.cursor = self.word_left(),
            KeyCode::Right if alt || ctrl => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.prev(self.cursor),
            KeyCode::Right => self.cursor = self.next(self.cursor),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Backspace => self.cut(self.prev(self.cursor), self.cursor),
            KeyCode::Delete => self.cut(self.cursor, self.next(self.cursor)),
            KeyCode::Char(c) if !ctrl && !alt => self.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Char(c) if alt && !ctrl && !c.is_ascii_alphabetic() => self.insert(c.encode_utf8(&mut [0; 4])),
            _ => return false,
        }
        true
    }

    /// 按字符边界删除正文，并解除或移动被影响的身份。
    fn cut(&mut self, from: usize, to: usize) {
        if from < to {
            self.move_bindings(from, to, "");
            self.text.replace_range(from..to, "");
            self.cursor = from;
        }
    }
}

pub(super) fn move_line(e: &mut Editor, d: isize) {
    let start = e.text[..e.cursor].rfind('\n').map_or(0, |i| i + 1);
    let col = e.text[start..e.cursor].chars().count();
    let target_start = if d < 0 {
        if start == 0 {
            return;
        }
        e.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
    } else {
        match e.text[e.cursor..].find('\n') {
            Some(i) => e.cursor + i + 1,
            None => return,
        }
    };
    let line_end = e.text[target_start..].find('\n').map_or(e.text.len(), |i| target_start + i);
    e.cursor = e.text[target_start..line_end].char_indices().nth(col).map_or(line_end, |(i, _)| target_start + i);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unicode 前缀不改变 UTF-16 身份，扩展名字或删除片段则解除绑定。
    #[test]
    fn edits_keep_only_unchanged_mention_tokens() {
        let mut e = Editor::with("😀@a tail");
        e.mentions.push(crate::chat::mentions::Mention { bot_id: "a".into(), start: 2, end: 4 });
        e.cursor = "😀@a".len();
        e.insert("!");
        assert_eq!((e.mentions[0].start, e.mentions[0].end), (2, 4));
        e.cursor = "😀@a".len();
        e.insert("字");
        assert_eq!(e.mentions, []);
        e.mentions.push(crate::chat::mentions::Mention { bot_id: "a".into(), start: 2, end: 4 });
        e.cursor = "😀@a".len();
        e.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(e.mentions, []);
    }

    #[test]
    fn editor_words_and_cuts() {
        let mut e = Editor::with("hello brave world");
        e.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(e.text, "hello brave ");
        e.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 0);
        e.key(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE));
        assert_eq!(e.text, "你hello brave ");
        e.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(e.text, "hello brave ");
    }

    #[test]
    fn line_moves_keep_the_column() {
        let mut e = Editor::with("abc\nde");
        move_line(&mut e, -1);
        assert_eq!(e.cursor, 2);
        move_line(&mut e, 1);
        assert_eq!(e.cursor, 6);
    }
}
