use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// A node in a Valve KeyValues tree.
#[derive(Debug, Clone, PartialEq)]
pub enum KvValue {
    String(String),
    Children(KvMap),
}

/// Ordered map of key-value pairs (preserves duplicate keys via Vec storage,
/// but BTreeMap gives sorted-key lookup).
pub type KvMap = BTreeMap<String, KvValue>;

impl KvValue {
    /// Get a child map by key, or `None`.
    pub fn get(&self, key: &str) -> Option<&KvValue> {
        match self {
            KvValue::Children(map) => map.get(key),
            _ => None,
        }
    }

    /// Get a string value by key, or `None`.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            KvValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get a child map by key, or `None`.
    pub fn get_children(&self, key: &str) -> Option<&KvMap> {
        match self.get(key)? {
            KvValue::Children(map) => Some(map),
            _ => None,
        }
    }

    /// Unwrap as a map reference.
    pub fn as_children(&self) -> Option<&KvMap> {
        match self {
            KvValue::Children(map) => Some(map),
            _ => None,
        }
    }

    /// Unwrap as a string reference.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            KvValue::String(s) => Some(s),
            _ => None,
        }
    }
}

/// Parse a Valve KeyValues text blob into a `KvValue::Children` root node.
///
/// The format is:
/// ```text
/// "key" "value"
/// "key" { ... }
/// ```
///
/// Supports escape sequences `\"`, `\\`, `\n`, `\t`.
pub fn parse(input: &[u8]) -> Result<KvValue> {
    // Strip trailing null bytes (Steam buffers are null-terminated).
    let trimmed = match input.iter().position(|&b| b == 0) {
        Some(pos) => &input[..pos],
        None => input,
    };
    let text = std::str::from_utf8(trimmed)
        .map_err(|e| Error::KvParse {
            offset: e.valid_up_to(),
            msg: "invalid utf-8".into(),
        })?;
    let mut parser = Parser { src: text, pos: 0 };
    let map = parser.parse_map()?;
    Ok(KvValue::Children(map))
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn parse_map(&mut self) -> Result<KvMap> {
        let mut map = KvMap::new();

        loop {
            self.skip_whitespace_and_comments();
            if self.pos >= self.src.len() || self.peek() == Some('}') {
                break;
            }

            let key = self.parse_string()?;
            self.skip_whitespace_and_comments();

            if self.peek() == Some('{') {
                self.pos += 1; // consume '{'
                let children = self.parse_map()?;
                self.expect('}')?;
                map.insert(key, KvValue::Children(children));
            } else {
                let value = self.parse_string()?;
                map.insert(key, KvValue::String(value));
            }
        }

        Ok(map)
    }

    fn parse_string(&mut self) -> Result<String> {
        self.skip_whitespace_and_comments();

        if self.peek() == Some('"') {
            self.parse_quoted_string()
        } else {
            self.parse_unquoted_string()
        }
    }

    fn parse_quoted_string(&mut self) -> Result<String> {
        self.pos += 1; // skip opening "
        let mut s = String::new();

        loop {
            let ch = self.next_char().ok_or_else(|| Error::KvParse {
                offset: self.pos,
                msg: "unterminated string".into(),
            })?;
            match ch {
                '"' => return Ok(s),
                '\\' => {
                    let esc = self.next_char().ok_or_else(|| Error::KvParse {
                        offset: self.pos,
                        msg: "unterminated escape".into(),
                    })?;
                    match esc {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                }
                other => s.push(other),
            }
        }
    }

    fn parse_unquoted_string(&mut self) -> Result<String> {
        let start = self.pos;
        while self.pos < self.src.len() {
            let ch = self.src.as_bytes()[self.pos];
            if ch.is_ascii_whitespace() || ch == b'{' || ch == b'}' || ch == b'"' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Error::KvParse {
                offset: self.pos,
                msg: format!("expected string, got {:?}", self.peek()),
            });
        }
        Ok(self.src[start..self.pos].to_string())
    }

    fn skip_whitespace_and_comments(&mut self) {
        while self.pos < self.src.len() {
            let ch = self.src.as_bytes()[self.pos];
            if ch.is_ascii_whitespace() {
                self.pos += 1;
            } else if ch == b'/' && self.src.as_bytes().get(self.pos + 1) == Some(&b'/') {
                // Line comment — skip to end of line
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.src[self.pos..].chars().next()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn expect(&mut self, ch: char) -> Result<()> {
        self.skip_whitespace_and_comments();
        if self.peek() == Some(ch) {
            self.pos += ch.len_utf8();
            Ok(())
        } else {
            Err(Error::KvParse {
                offset: self.pos,
                msg: format!("expected '{}', got {:?}", ch, self.peek()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple() {
        let input = br#"
            "appinfo"
            {
                "appid"  "730"
                "common"
                {
                    "name"  "Counter-Strike 2"
                    "type"  "Game"
                }
            }
        "#;
        let root = parse(input).unwrap();
        let appinfo = root.get("appinfo").unwrap();
        assert_eq!(appinfo.get_str("appid"), Some("730"));
        assert_eq!(
            appinfo.get("common").unwrap().get_str("name"),
            Some("Counter-Strike 2")
        );
    }

    #[test]
    fn parse_escapes() {
        let input = br#""key" "hello \"world\" \\end""#;
        let root = parse(input).unwrap();
        assert_eq!(root.get_str("key"), Some("hello \"world\" \\end"));
    }
}
