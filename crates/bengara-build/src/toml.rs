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

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }

        // 節の始まり。
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("{line_number} 行目: 節の名前が空です"));
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
        let value = unquote(value.trim())
            .ok_or_else(|| format!("{line_number} 行目: 値を引用符でくくってください"))?;

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
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match (quote, b) {
            (None, b'"') | (None, b'\'') => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b'#') => return &line[..i],
            _ => {}
        }
    }
    line
}

/// 引用符を外す。`"..."` は `\n` などを解釈し、`'...'` はそのままです。
fn unquote(value: &str) -> Option<String> {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return Some(inner.to_string());
    }
    let inner = value.strip_prefix('"').and_then(|v| v.strip_suffix('"'))?;

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
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
    fn 読めない行は行番号を知らせる() {
        assert!(parse("[m]\nこれは鍵だけ\n").unwrap_err().contains("2 行目"));
        assert!(parse("[m]\na = 引用符なし\n")
            .unwrap_err()
            .contains("2 行目"));
        assert!(parse("[]\n").unwrap_err().contains("1 行目"));
        assert!(parse("= 値\n").unwrap_err().contains("1 行目"));
    }

    #[test]
    fn 空の入力は空の一覧() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("\n\n# コメントだけ\n").unwrap().is_empty());
    }
}
