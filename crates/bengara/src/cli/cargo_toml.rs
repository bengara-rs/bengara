//! `Cargo.toml` の最小限の読み書き。
//!
//! `init` が足りない行を足すためだけの処理です。TOML を完全に解釈はしません。
//! すでに書かれている行には触らず、無いものだけを足します。

use std::path::Path;

use crate::error::{Error, Result};

/// 行の並びとして見た `Cargo.toml`。
pub(crate) struct CargoToml {
    lines: Vec<String>,
    /// 元の内容から変わったか。
    changed: bool,
}

impl CargoToml {
    /// 読み込む。
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::msg(format!("{} を読めません: {e}", path.display())))?;
        Ok(Self {
            lines: text.lines().map(str::to_string).collect(),
            changed: false,
        })
    }

    /// 変わっていれば書き戻す。
    pub(crate) fn save(&self, path: &Path) -> Result<bool> {
        if !self.changed {
            return Ok(false);
        }
        let mut text = self.lines.join("\n");
        text.push('\n');
        std::fs::write(path, text)
            .map_err(|e| Error::msg(format!("{} を書けません: {e}", path.display())))?;
        Ok(true)
    }

    /// `[package]` の `name`。
    pub(crate) fn package_name(&self) -> Option<String> {
        self.value_in("package", "name")
    }

    /// 節の中のキーの値（文字列として書かれている前提）。
    pub(crate) fn value_in(&self, section: &str, key: &str) -> Option<String> {
        let range = self.section_range(section)?;
        for line in &self.lines[range] {
            if let Some(value) = key_of(line).filter(|k| k == key).and(line.split_once('=')) {
                return Some(value.1.trim().trim_matches('"').to_string());
            }
        }
        None
    }

    /// 節があるか。
    pub(crate) fn has_section(&self, section: &str) -> bool {
        self.section_range(section).is_some()
    }

    /// 節の中にキーがあるか。
    pub(crate) fn has_key(&self, section: &str, key: &str) -> bool {
        self.value_in(section, key).is_some()
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

    /// その文字列が どこかに書かれているか。
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.lines.iter().any(|line| line.contains(needle))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> CargoToml {
        CargoToml {
            lines: text.lines().map(str::to_string).collect(),
            changed: false,
        }
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
}
