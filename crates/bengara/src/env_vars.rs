//! `.env` の読み込みと、型付きの環境変数の取り出し。
//!
//! 実際の環境変数が `.env` より優先されます（本番で環境変数だけを使えるように）。

use std::path::Path;

/// 環境変数を型で受け取る。見つからない・変換できないときは `default` を返す。
///
/// ```ignore
/// AppConfig {
///     name: env("APP_NAME", "myapp"),
///     debug: env("APP_DEBUG", false),
/// }
/// ```
///
/// **空文字は未設定と同じ扱いです。** 前後の空白を落として空になった値も含みます。
/// `.env` に `MAIL_FROM_NAME=` と書いても `default` が入るので、
/// **この書き方で値を空にはできません。** 空にしたいときは、設定を読む側で
/// `env("MAIL_FROM_NAME", "")` のように既定値そのものを空にしてください。
pub fn env<T: FromEnv>(key: &str, default: impl Into<T>) -> T {
    match std::env::var(key) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return default.into();
            }
            match T::from_env(trimmed) {
                Some(value) => value,
                None => {
                    tracing::warn!(
                        "環境変数 {key} の値 `{trimmed}` を変換できないため、既定値を使います"
                    );
                    default.into()
                }
            }
        }
        Err(_) => default.into(),
    }
}

/// 環境変数の文字列から値を作る。
pub trait FromEnv: Sized {
    /// 変換できないときは `None`。
    fn from_env(raw: &str) -> Option<Self>;
}

impl FromEnv for String {
    fn from_env(raw: &str) -> Option<Self> {
        Some(raw.to_string())
    }
}

impl FromEnv for bool {
    fn from_env(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" | "null" | "none" => Some(false),
            _ => None,
        }
    }
}

impl FromEnv for std::path::PathBuf {
    fn from_env(raw: &str) -> Option<Self> {
        Some(std::path::PathBuf::from(raw))
    }
}

macro_rules! impl_from_env_parse {
    ($($t:ty),* $(,)?) => {
        $(impl FromEnv for $t {
            fn from_env(raw: &str) -> Option<Self> { raw.parse().ok() }
        })*
    };
}

impl_from_env_parse!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64);

/// `.env` を読んで、まだ設定されていない環境変数だけを設定する。
///
/// 戻り値は読み込んだファイルのパス（無ければ `None`）。
pub(crate) fn load_dotenv(base: &Path) -> Option<std::path::PathBuf> {
    let path = base.join(".env");
    let text = std::fs::read_to_string(&path).ok()?;
    for (key, value) in parse_dotenv(&text) {
        if std::env::var_os(&key).is_none() {
            // SAFETY: 起動直後、スレッドを作る前に1回だけ呼ぶ。
            // edition 2021 では set_var は安全な関数なので unsafe は余る。
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var(&key, &value)
            };
        }
    }
    Some(path)
}

/// `.env` の中身を `KEY=VALUE` の並びに変える。
///
/// - `#` から行末まではコメント（引用符の中は除く）
/// - 先頭の `export ` は無視する
/// - 値は `"..."` と `'...'` で囲める。`"` の中では `\n` `\t` `\\` `\"` を解釈する
fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        out.push((key.to_string(), parse_value(rest.trim())));
    }
    out
}

fn parse_value(raw: &str) -> String {
    let mut chars = raw.chars().peekable();
    match chars.peek() {
        Some('"') => {
            chars.next();
            let mut out = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('r') => out.push('\r'),
                        Some('t') => out.push('\t'),
                        Some(other) => out.push(other),
                        None => break,
                    },
                    other => out.push(other),
                }
            }
            out
        }
        Some('\'') => {
            chars.next();
            let mut out = String::new();
            for c in chars {
                if c == '\'' {
                    break;
                }
                out.push(c);
            }
            out
        }
        _ => {
            // 引用符なしの値は、`#` より前までを取り、前後の空白を落とす。
            let value = match raw.find(" #") {
                Some(i) => &raw[..i],
                None => raw,
            };
            value.trim().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 基本的な行を読める() {
        let v = parse_dotenv("APP_NAME=myapp\nAPP_DEBUG=true\n");
        assert_eq!(
            v,
            vec![
                ("APP_NAME".to_string(), "myapp".to_string()),
                ("APP_DEBUG".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn コメントと空行を飛ばす() {
        let v = parse_dotenv("# comment\n\nA=1 # 末尾のコメント\n");
        assert_eq!(v, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn 引用符を外す() {
        let v = parse_dotenv("A=\"hello world\"\nB='it # is'\n");
        assert_eq!(v[0].1, "hello world");
        assert_eq!(v[1].1, "it # is");
    }

    #[test]
    fn exportを無視する() {
        let v = parse_dotenv("export A=1\n");
        assert_eq!(v, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn 真偽値の変換() {
        assert_eq!(bool::from_env("TRUE"), Some(true));
        assert_eq!(bool::from_env("off"), Some(false));
        assert_eq!(bool::from_env("maybe"), None);
    }
}
