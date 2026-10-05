//! `Cargo.toml` の最小限の読み書き。
//!
//! `init` が足りない行を足すためだけの処理です。TOML を完全に解釈はしません。
//! すでに書かれている行には触らず、無いものだけを足します。

use std::path::Path;

use crate::error::{Error, Result};

/// 行の並びとして見た `Cargo.toml`。
pub(crate) struct CargoToml {
    lines: Vec<String>,
    /// 元のファイルの改行（`\n` か `\r\n`）。書き戻すときに保ちます。
    newline: &'static str,
    /// 元の内容から変わったか。
    changed: bool,
}

impl CargoToml {
    /// 読み込む。
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::msg(format!("{} を読めません: {e}", path.display())))?;
        Ok(Self::from_text(&text))
    }

    /// 文字列から組み立てる。
    fn from_text(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_string).collect(),
            // 1行足すだけで全行が差分になるのを避けるため、元の改行に合わせる。
            newline: if text.contains("\r\n") { "\r\n" } else { "\n" },
            changed: false,
        }
    }

    /// 変わっていれば書き戻す。
    pub(crate) fn save(&self, path: &Path) -> Result<bool> {
        if !self.changed {
            return Ok(false);
        }
        let mut text = self.lines.join(self.newline);
        text.push_str(self.newline);
        std::fs::write(path, text)
            .map_err(|e| Error::msg(format!("{} を書けません: {e}", path.display())))?;
        Ok(true)
    }

    /// 文字列から組み立てる（テスト用）。
    #[cfg(test)]
    pub(crate) fn load_for_test(text: &str) -> Self {
        Self::from_text(text)
    }

    /// `[package]` の `name`。
    pub(crate) fn package_name(&self) -> Option<String> {
        self.value_in("package", "name")
    }

    /// 節の中のキーの値（文字列として書かれている前提）。
    ///
    /// 値の後ろのコメントは落とします（`name = "myapp" # 開発用` → `myapp`）。
    pub(crate) fn value_in(&self, section: &str, key: &str) -> Option<String> {
        let range = self.section_range(section)?;
        for line in &self.lines[range] {
            if key_of(line).is_some_and(|k| k == key) {
                return value_of(line);
            }
        }
        None
    }

    /// 節があるか。
    pub(crate) fn has_section(&self, section: &str) -> bool {
        self.section_range(section).is_some()
    }

    /// 節の中にキーがあるか。
    ///
    /// `[dependencies] bengara = {...}` と `[dependencies.bengara]` のどちらの形でも
    /// 見つけます。見落とすと `init` が同じキーを2回書いて、Cargo が落ちます。
    pub(crate) fn has_key(&self, section: &str, key: &str) -> bool {
        self.value_in(section, key).is_some() || self.has_section(&format!("{section}.{key}"))
    }

    /// `path = "main.rs"` のような行があるか。
    ///
    /// 空白と引用符の書き方の違いは無視します（`path="main.rs"` も `path = 'main.rs'` も同じ）。
    pub(crate) fn has_path(&self, file: &str) -> bool {
        self.lines.iter().any(|line| {
            key_of(line).is_some_and(|k| k == "path") && value_of(line).as_deref() == Some(file)
        })
    }

    /// 節の最後に行を足す。節が無ければ作る。
    pub(crate) fn add_line(&mut self, section: &str, line: &str) {
        match self.section_range(section) {
            Some(range) => {
                // 節の中の、空行でない最後の行の次に入れる。
                let mut at = range.end;
                while at > range.start && self.lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                self.lines.insert(at, line.to_string());
            }
            None => {
                if !self.lines.last().is_some_and(|l| l.trim().is_empty()) {
                    self.lines.push(String::new());
                }
                self.lines.push(format!("[{section}]"));
                self.lines.push(line.to_string());
            }
        }
        self.changed = true;
    }

    /// その文字列が どこかに書かれているか（テスト用）。
    #[cfg(test)]
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.lines.iter().any(|line| line.contains(needle))
    }

    /// その文字列を含む行の数（テスト用）。
    #[cfg(test)]
    pub(crate) fn count_lines(&self, needle: &str) -> usize {
        self.lines
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// 末尾に行の塊を足す（`[[bin]]` のような繰り返しの節のため）。
    pub(crate) fn append_block(&mut self, block: &[&str]) {
        if !self.lines.last().is_some_and(|l| l.trim().is_empty()) {
            self.lines.push(String::new());
        }
        for line in block {
            self.lines.push((*line).to_string());
        }
        self.changed = true;
    }

    /// 節の中身（ヘッダー行の次から、次のヘッダー行まで）。
    fn section_range(&self, section: &str) -> Option<std::ops::Range<usize>> {
        let header = format!("[{section}]");
        let start = self.lines.iter().position(|l| l.trim() == header)? + 1;
        let end = self.lines[start..]
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .map(|i| start + i)
            .unwrap_or(self.lines.len());
        Some(start..end)
    }
}

/// `key = value` の `key`。コメント行と空行は `None`。
fn key_of(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once('=')?;
    Some(key.trim().to_string())
}

/// `key = value` の `value`。行末のコメントと引用符を外します。
fn value_of(line: &str) -> Option<String> {
    let (_, value) = line.split_once('=')?;
    Some(unquote(strip_comment(value).trim()).to_string())
}

/// 値から、引用符の外にある `#` 以降を落とす。
///
/// 引用符の中の `#` は値の一部です（`name = "a#b"`）。
fn strip_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match (quote, b) {
            // 二重引用符の中の `\` は、次の1文字を打ち消す。
            (Some(b'"'), b'\\') => i += 1,
            (None, b'"') | (None, b'\'') => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b'#') => return &value[..i],
            _ => {}
        }
        i += 1;
    }
    value
}

/// 前後の引用符を外す（`"myapp"` と `'myapp'` のどちらも）。
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> CargoToml {
        CargoToml::from_text(text)
    }

    #[test]
    fn パッケージ名を読める() {
        let d = doc("[package]\nname = \"myapp\"\nversion = \"0.1.0\"\n");
        assert_eq!(d.package_name().as_deref(), Some("myapp"));
    }

    #[test]
    fn 節の有無を調べられる() {
        let d = doc("[package]\nname = \"myapp\"\n\n[dependencies]\nbengara = \"0.1\"\n");
        assert!(d.has_section("dependencies"));
        assert!(!d.has_section("build-dependencies"));
        assert!(d.has_key("dependencies", "bengara"));
        assert!(!d.has_key("dependencies", "serde"));
    }

    #[test]
    fn 節の最後に足せる() {
        let mut d = doc("[package]\nname = \"myapp\"\n\n[dependencies]\nbengara = \"0.1\"\n");
        d.add_line("package", "default-run = \"myapp\"");
        assert_eq!(
            d.lines.join("\n"),
            "[package]\nname = \"myapp\"\ndefault-run = \"myapp\"\n\n[dependencies]\nbengara = \"0.1\""
        );
    }

    #[test]
    fn 無い節は作る() {
        let mut d = doc("[package]\nname = \"myapp\"\n");
        d.add_line("build-dependencies", "bengara-build = \"0.1\"");
        assert!(d
            .lines
            .join("\n")
            .contains("[build-dependencies]\nbengara-build = \"0.1\""));
    }

    #[test]
    fn 節の形で書いた依存も見つける() {
        let d = doc(
            "[package]\nname = \"myapp\"\n\n[dependencies.bengara]\nversion = \"0.1\"\nfeatures = [\"sqlite\"]\n",
        );
        assert!(d.has_key("dependencies", "bengara"));
        assert!(!d.has_key("dependencies", "serde"));
    }

    #[test]
    fn 値の後ろのコメントは落とす() {
        let d = doc("[package]\nname = \"myapp\" # 開発用\n");
        assert_eq!(d.package_name().as_deref(), Some("myapp"));
        let d = doc("[package]\nname = \"my#app\"\n");
        assert_eq!(d.package_name().as_deref(), Some("my#app"));
    }

    #[test]
    fn 空白や引用符が違ってもbinのpathを見つける() {
        let d = doc("[[bin]]\nname = \"myapp\"\npath=\"main.rs\"\n");
        assert!(d.has_path("main.rs"));
        let d = doc("[[bin]]\npath = 'main.rs'  # 入口\n");
        assert!(d.has_path("main.rs"));
        let d = doc("[[bin]]\npath = \"artisan.rs\"\n");
        assert!(!d.has_path("main.rs"));
        // コメント行は数えない。
        let d = doc("# path = \"main.rs\"\n");
        assert!(!d.has_path("main.rs"));
    }

    #[test]
    fn 元の改行を保つ() {
        let mut d = doc("[package]\r\nname = \"myapp\"\r\n");
        d.add_line("package", "autobins = false");
        assert_eq!(
            d.lines.join(d.newline),
            "[package]\r\nname = \"myapp\"\r\nautobins = false"
        );
        let mut d = doc("[package]\nname = \"myapp\"\n");
        d.add_line("package", "autobins = false");
        assert_eq!(d.newline, "\n");
    }
}
