//! TOML の最小限の読み取り。
//!
//! 言語ファイル（`resources/lang/*.toml`）を読むためだけの処理です。ビルド時だけ動きます。
//! TOML を完全に解釈はしません。読むのは次の形だけです。
//!
//! ```toml
//! # コメント
//! [messages]
//! welcome = "ようこそ"
//! greeting = 'こんにちは、:name さん'
//! ```
//!
//! - 節（`[messages]`）の中の `key = "値"` を `messages.welcome` という鍵にします。
//! - 節の外に書いた `key = "値"` は、そのままの鍵になります。
//! - 値は**必ず引用符でくくってください**。数や真偽をそのまま書くとエラーにします。
//!   `"` の中では `\n` `\t` `\r` `\\` `\"` が使えます。
//! - 入れ子の節（`[a.b]`）は、そのまま `a.b.key` になります。
//!
//! 依存クレートを増やさないために自前で書いています。

/// 1行ずつ読んで `(鍵, 値)` の一覧にする。
///
/// 読めない行があれば、その行番号と理由を `Err` で返します。
pub(crate) fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut section = String::new();

    // Windows のメモ帳や「UTF-8 with BOM」で保存すると、先頭に BOM が付く。
    // 残すと1行目が `\u{feff}[messages]` になり、`[` で始まらないので
    // 「1 行目: `鍵 = "値"` の形ではありません」という見当違いのエラーになっていた。
    // `.env` を読む側（`env_vars.rs`）と同じやり方で外す。
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }

        // 節の始まり。`[` で始まる行は、`]` で終わらなければその場でエラーにします。
        // ここで落とさずに `鍵 = 値` の処理へ送ると、`[a] b = "c"` のような行が
        // `[a] b` という引けない鍵になって通ってしまいます。
        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                return Err(format!(
                    "{line_number} 行目: 節は `[名前]` の形にしてください"
                ));
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("{line_number} 行目: 節の名前が空です"));
            }
            if !is_key(name) {
                return Err(format!(
                    "{line_number} 行目: 節の名前 `{name}` は使えません。\
                     英数字・`_`・`-`・`.` だけにしてください（`.` は区切りなので端や連続では使えません）"
                ));
            }
            section = name.to_string();
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "{line_number} 行目: `鍵 = \"値\"` の形ではありません"
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("{line_number} 行目: 鍵が空です"));
        }
        if !is_key(key) {
            return Err(format!(
                "{line_number} 行目: 鍵 `{key}` は使えません。\
                 英数字・`_`・`-`・`.` だけにしてください（引用符でくくる書き方は読みません）"
            ));
        }
        let value = unquote(value.trim()).ok_or_else(|| {
            format!(
                "{line_number} 行目: 値は `\"値\"` の形にしてください。\
                 引用符でくくり、中の `\"` は `\\\"` にします"
            )
        })?;

        let full = if section.is_empty() {
            key.to_string()
        } else {
            format!("{section}.{key}")
        };
        out.push((full, value));
    }
    Ok(out)
}

/// 行から `#` 以降を落とす。ただし引用符の中の `#` は残す。
///
/// 二重引用符の中の `\` は、次の1文字を打ち消します。`\"` を終わりと数えると
/// 引用の内と外がずれて、まるで違う理由のエラーになるためです。
/// 単引用符の中では `\` はそのままの文字なので、飛ばしません。
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match (quote, b) {
            (Some(b'"'), b'\\') => i += 1,
            (None, b'"') | (None, b'\'') => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b'#') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// 鍵と節の名前に使える形か。
///
/// 使えるのは英数字・`_`・`-`・`.` だけです。`.` は鍵の区切りなので、端に置いたり
/// 続けたりはできません。`[a.]` を通すと `a..b` という、実行時に引けない鍵になります。
fn is_key(name: &str) -> bool {
    if name.starts_with('.') || name.ends_with('.') || name.contains("..") {
        return false;
    }
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// 引用符を外す。`"..."` は `\n` などを解釈し、`'...'` はそのままです。
///
/// 閉じの引用符は**行の終わり**になければなりません。途中で閉じていると、
/// `a = "x""y"` のような行が `x""y` という値になって通ってしまいます。
fn unquote(value: &str) -> Option<String> {
    if let Some(rest) = value.strip_prefix('\'') {
        let inner = rest.strip_suffix('\'')?;
        // 単引用符の中では `\` も文字なので、`'` が出たらそこで値が終わっています。
        if inner.contains('\'') {
            return None;
        }
        return Some(inner.to_string());
    }
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        // 逃がしていない `"`。ここで値が終わっていて、後ろに余りがあります。
        if c == '"' {
            return None;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            // 知らない逃げ方は、そのまま2文字として残す。
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 節の中の鍵は点でつなぐ() {
        let pairs = parse(
            "[messages]\nwelcome = \"ようこそ\"\ngreeting = \"こんにちは\"\n\n[validation]\nrequired = \"必須です\"\n",
        )
        .unwrap();
        assert_eq!(
            pairs,
            vec![
                ("messages.welcome".to_string(), "ようこそ".to_string()),
                ("messages.greeting".to_string(), "こんにちは".to_string()),
                ("validation.required".to_string(), "必須です".to_string()),
            ]
        );
    }

    #[test]
    fn 節の外の鍵はそのまま() {
        let pairs = parse("title = \"題名\"\n").unwrap();
        assert_eq!(pairs, vec![("title".to_string(), "題名".to_string())]);
    }

    #[test]
    fn 入れ子の節も読める() {
        let pairs = parse("[a.b]\nc = \"値\"\n").unwrap();
        assert_eq!(pairs[0].0, "a.b.c");
    }

    #[test]
    fn コメントと空行は飛ばす() {
        let pairs = parse("# これはコメント\n\n[m]\na = \"1\"  # 行の後ろのコメント\n").unwrap();
        assert_eq!(pairs, vec![("m.a".to_string(), "1".to_string())]);
    }

    #[test]
    fn 引用符の中のシャープは残る() {
        let pairs = parse("[m]\na = \"色 #ff0000\"\n").unwrap();
        assert_eq!(pairs[0].1, "色 #ff0000");
    }

    #[test]
    fn 単引用符はそのままの文字() {
        let pairs = parse("[m]\na = 'そのまま\\nです'\n").unwrap();
        assert_eq!(pairs[0].1, "そのまま\\nです");
    }

    #[test]
    fn 二重引用符は逃げ方を解釈する() {
        let pairs = parse("[m]\na = \"1行目\\n2行目\\t終わり\"\n").unwrap();
        assert_eq!(pairs[0].1, "1行目\n2行目\t終わり");
    }

    #[test]
    fn 逃がした引用符と行末のコメントが同じ行にあっても読める() {
        let pairs = parse("[m]\na = \"引用\\\"符\" # コメント\n").unwrap();
        assert_eq!(pairs[0].1, "引用\"符");
    }

    #[test]
    fn 単引用符の中の円記号はそのまま() {
        // 単引用符の中では `\` は文字。`\'` で終わりが消えたりしない。
        let pairs = parse("[m]\na = 'C:\\path\\' # コメント\n").unwrap();
        assert_eq!(pairs[0].1, "C:\\path\\");
    }

    #[test]
    fn 読めない行は行番号を知らせる() {
        assert!(parse("[m]\nこれは鍵だけ\n").unwrap_err().contains("2 行目"));
        assert!(parse("[m]\na = 引用符なし\n")
            .unwrap_err()
            .contains("2 行目"));
        assert!(parse("[]\n").unwrap_err().contains("1 行目"));
        assert!(parse("= 値\n").unwrap_err().contains("1 行目"));
    }

    #[test]
    fn 節として読めない行はエラーにする() {
        // `[a] b = "c"` は `[a] b` という引けない鍵になっていた。
        assert!(parse("[a] b = \"c\"\n").unwrap_err().contains("1 行目"));
        // `[a.]` は `a..b` という引けない鍵になっていた。
        assert!(parse("[a.]\nb = \"1\"\n").unwrap_err().contains("1 行目"));
    }

    #[test]
    fn 使えない鍵と閉じ忘れの値はエラーにする() {
        // 引用符でくくった鍵は読まない。`"a.b"` が鍵のまま残っていた。
        assert!(parse("\"a.b\" = \"x\"\n").unwrap_err().contains("1 行目"));
        // 値の途中で引用符が閉じている。`x""y` という値になっていた。
        assert!(parse("a = \"x\"\"y\"\n").unwrap_err().contains("1 行目"));
        // 単引用符も同じ。
        assert!(parse("a = 'x''y'\n").unwrap_err().contains("1 行目"));
    }

    #[test]
    fn 鍵の形を見分ける() {
        assert!(is_key("messages"));
        assert!(is_key("a.b"));
        assert!(is_key("a_b-c2"));
        assert!(!is_key(""));
        assert!(!is_key("a."));
        assert!(!is_key(".a"));
        assert!(!is_key("a..b"));
        assert!(!is_key("\"a.b\""));
        assert!(!is_key("a b"));
        assert!(!is_key("日本語"));
    }

    #[test]
    fn 先頭のbomは外す() {
        // 以前は BOM が残り、1行目が `\u{feff}[messages]` になって
        // 「`鍵 = "値"` の形ではありません」という見当違いのエラーになっていた。
        let pairs = parse("\u{feff}[messages]\nwelcome = \"ようこそ\"\n").unwrap();
        assert_eq!(
            pairs,
            vec![("messages.welcome".to_string(), "ようこそ".to_string())]
        );
        // 節の外の鍵でも同じ。
        let pairs = parse("\u{feff}title = \"題名\"\n").unwrap();
        assert_eq!(pairs, vec![("title".to_string(), "題名".to_string())]);
        // BOM だけのファイルは空の一覧。
        assert!(parse("\u{feff}").unwrap().is_empty());
    }

    #[test]
    fn 空の入力は空の一覧() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("\n\n# コメントだけ\n").unwrap().is_empty());
    }
}
