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
        for line in self.plain_lines(range) {
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
    /// 次のどの形でも見つけます。見落とすと `init` が同じキーを2回書いて、Cargo が落ちます。
    ///
    /// | 形 | 例 |
    /// |---|---|
    /// | そのまま | `bengara = "0.1"` |
    /// | 引用符つき | `"bengara" = "0.1"` |
    /// | ドット付き（workspace 継承） | `bengara.workspace = true` |
    /// | 節の形 | `[dependencies.bengara]` |
    pub(crate) fn has_key(&self, section: &str, key: &str) -> bool {
        if self.has_section(&format!("{section}.{key}")) {
            return true;
        }
        let Some(range) = self.section_range(section) else {
            return false;
        };
        // `rust-version.workspace = true` のような、キーの後ろにドットが続く形も見る。
        let dotted = format!("{key}.");
        self.plain_lines(range)
            .into_iter()
            .any(|line| key_of(line).is_some_and(|k| k == key || k.starts_with(&dotted)))
    }

    /// `path = "main.rs"` のような行があるか。
    ///
    /// 空白と引用符の書き方の違いは無視します（`path="main.rs"` も `path = 'main.rs'` も同じ）。
    pub(crate) fn has_path(&self, file: &str) -> bool {
        self.plain_lines(0..self.lines.len())
            .into_iter()
            .any(|line| {
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

    /// 節の中身（見出し行の次から、次の見出し行まで）。
    ///
    /// 見出しと認めるのは、**行全体が `[...]` の形**で、複数行文字列の中でも
    /// 配列リテラルの中でもない行だけです。以前は「trim 後が `[` で始まる行」で
    /// 区切っていたので、次の `[note] see docs` を節の終わりと見なしていました。
    ///
    /// ```toml
    /// description = """
    /// [note] see docs
    /// """
    /// default-run = "myapp"
    /// ```
    ///
    /// その結果 `has_key("package", "default-run")` が偽になり、`init` が
    /// `default-run` を二重に足して cargo が拒否していました。
    fn section_range(&self, section: &str) -> Option<std::ops::Range<usize>> {
        let kinds = self.classify();
        let header = format!("[{section}]");
        let start = kinds
            .iter()
            .position(|kind| matches!(kind, LineKind::Header(found) if *found == header))?
            + 1;
        let end = kinds[start..]
            .iter()
            .position(|kind| matches!(kind, LineKind::Header(_)))
            .map(|i| start + i)
            .unwrap_or(self.lines.len());
        Some(start..end)
    }

    /// その範囲のうち、`key = value` として読める行だけ。
    ///
    /// 複数行文字列の中の行と、閉じていない `[` の中の行（配列リテラルの続き）は
    /// 外します。中に `a = 1` のような形が入っていても、キーとして読まないためです。
    fn plain_lines(&self, range: std::ops::Range<usize>) -> Vec<&String> {
        let kinds = self.classify();
        self.lines[range.clone()]
            .iter()
            .zip(&kinds[range])
            .filter(|(_, kind)| matches!(kind, LineKind::Plain))
            .map(|(line, _)| line)
            .collect()
    }

    /// 1行ずつ、見出し・ふつうの行・続きの行に分ける。
    fn classify(&self) -> Vec<LineKind> {
        let mut out = Vec::with_capacity(self.lines.len());
        let mut scan = Scan::default();
        for line in &self.lines {
            // 種類は**行の頭の状態**で決める。その行を読み進めるのは後。
            let kind = if scan.inside() {
                LineKind::Inside
            } else if let Some(header) = header_text(line) {
                LineKind::Header(header)
            } else {
                LineKind::Plain
            };
            scan.feed(line);
            out.push(kind);
        }
        out
    }
}

/// 行の種類。
#[derive(Debug, PartialEq, Eq)]
enum LineKind {
    /// 行全体が `[...]` の見出し（`[package]`・`[[bin]]`）。かっこを含む文字を持ちます。
    Header(String),
    /// 節の中のふつうの行。
    Plain,
    /// 複数行文字列の中、または閉じていない `[` の中。
    Inside,
}

/// 行全体が `[...]` の形なら、その文字（行末のコメントを落としたもの）を返す。
fn header_text(line: &str) -> Option<String> {
    let trimmed = strip_comment(line).trim();
    if trimmed.len() >= 2 && trimmed.starts_with('[') && trimmed.ends_with(']') {
        return Some(trimmed.to_string());
    }
    None
}

/// 上から1行ずつ読むときの状態。
#[derive(Default)]
struct Scan {
    /// 複数行文字列の中なら、その閉じ記号（`"""` か `'''`）。
    multiline: Option<&'static str>,
    /// 閉じていない `[` の数。
    depth: usize,
}

impl Scan {
    /// いま、行の中身を読む対象にしない状態か。
    fn inside(&self) -> bool {
        self.multiline.is_some() || self.depth > 0
    }

    /// 1行ぶん読み進める。
    ///
    /// 作りは `strip_comment` と同じです（引用符の中は中身として飛ばし、外の `#` で打ち切る）。
    /// 違いは、複数行文字列（`"""` と `'''`）と角かっこの深さも数える点です。
    fn feed(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut i = 0;

        // 複数行文字列の続き。閉じ記号が出るまでは何も数えない。
        if let Some(close) = self.multiline {
            let Some(found) = find_from(bytes, close.as_bytes(), 0) else {
                return;
            };
            self.multiline = None;
            i = found + close.len();
        }

        while i < bytes.len() {
            match bytes[i] {
                // 引用符の外の `#` から後ろはコメント。
                b'#' => return,
                b'[' => {
                    self.depth += 1;
                    i += 1;
                }
                b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    i += 1;
                }
                quote @ (b'"' | b'\'') => {
                    let triple: &'static str = if quote == b'"' { "\"\"\"" } else { "'''" };
                    if bytes[i..].starts_with(triple.as_bytes()) {
                        match find_from(bytes, triple.as_bytes(), i + triple.len()) {
                            // 同じ行で閉じている。
                            Some(found) => i = found + triple.len(),
                            None => {
                                self.multiline = Some(triple);
                                return;
                            }
                        }
                    } else {
                        i = skip_string(bytes, i);
                    }
                }
                _ => i += 1,
            }
        }
    }
}

/// 1行の中の文字列を飛ばす。`start` は開きの引用符の位置。
///
/// 戻すのは閉じの引用符の次の位置です。閉じていなければ行末を返します。
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        // 二重引用符の中の `\` は、次の1文字を打ち消す。
        if quote == b'"' && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// `from` から後ろで `needle` が最初に出る位置。
fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| position + from)
}

/// `key = value` の `key`。コメント行と空行は `None`。
///
/// `"bengara" = "0.1"` のような引用符つきのキーは、引用符を外して返します。
fn key_of(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once('=')?;
    Some(unquote(key.trim()).to_string())
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
    fn workspace継承のキーも見つける() {
        // 見落とすと init が rust-version を2回書き、cargo が動かなくなる。
        let d = doc("[package]\nname = \"myapp\"\nrust-version.workspace = true\n");
        assert!(d.has_key("package", "rust-version"));
        assert!(!d.has_key("package", "default-run"));

        let d = doc("[dependencies]\nbengara.workspace = true\n");
        assert!(d.has_key("dependencies", "bengara"));
        // 前方一致だけで判断しない（`bengara-build` は別のキー）。
        assert!(!d.has_key("dependencies", "bengara-build"));
    }

    #[test]
    fn 引用符つきのキーも見つける() {
        let d = doc("[dependencies]\n\"bengara\" = \"0.1\"\n");
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
    fn 複数行文字列の中の角かっこは節の境目にしない() {
        // 以前は `[note] see docs` を境目と見なし、`default-run` を見落としていた。
        // その結果 init が同じキーを2回書き、cargo が拒否していた。
        let d = doc(concat!(
            "[package]\n",
            "name = \"myapp\"\n",
            "description = \"\"\"\n",
            "[note] see docs\n",
            "\"\"\"\n",
            "default-run = \"myapp\"\n",
            "\n",
            "[dependencies]\n",
            "bengara = \"0.1\"\n",
        ));
        assert!(d.has_key("package", "default-run"));
        assert!(d.has_key("package", "description"));
        assert_eq!(d.package_name().as_deref(), Some("myapp"));
        // 文字列の中の行はキーとして読まない。
        assert!(!d.has_key("package", "[note] see docs"));
        assert!(d.has_section("dependencies"));
        assert!(d.has_key("dependencies", "bengara"));
    }

    #[test]
    fn 入れ子の配列の中には挿入しない() {
        // 以前は `[\"a\"],` の行を節の終わりと見なし、配列リテラルの中に行を入れて
        // ファイルを壊していた。
        let mut d = doc(concat!(
            "[package]\n",
            "name = \"myapp\"\n",
            "foo = [\n",
            "  [\"a\"],\n",
            "  [\"b\"],\n",
            "]\n",
        ));
        assert!(!d.has_key("package", "default-run"));
        d.add_line("package", "default-run = \"myapp\"");
        assert_eq!(
            d.lines.join("\n"),
            concat!(
                "[package]\n",
                "name = \"myapp\"\n",
                "foo = [\n",
                "  [\"a\"],\n",
                "  [\"b\"],\n",
                "]\n",
                "default-run = \"myapp\""
            )
        );
    }

    #[test]
    fn 見出しは行全体が角かっこの形のときだけ() {
        // 値の中の `[` では区切らない。
        let d = doc("[package]\nname = \"a[b]c\"\nversion = \"0.1.0\"\n");
        assert!(d.has_key("package", "version"));
        // 行末のコメントが付いた見出しも見出しとして読む。
        let d = doc("[package] # 中身\nname = \"myapp\"\n");
        assert_eq!(d.package_name().as_deref(), Some("myapp"));
        // `[[bin]]` も境目になる。
        let d = doc("[package]\nname = \"myapp\"\n\n[[bin]]\nname = \"artisan\"\n");
        assert!(!d.has_key("package", "path"));
        assert!(d.has_section("package"));
    }

    #[test]
    fn 一行で閉じた複数行文字列は状態を残さない() {
        let d = doc("[package]\nname = \"\"\"myapp\"\"\"\nversion = \"0.1.0\"\n");
        assert!(d.has_key("package", "version"));
        // 単引用符の三連も同じ。
        let d = doc("[package]\nname = '''myapp'''\nversion = \"0.1.0\"\n");
        assert!(d.has_key("package", "version"));
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
