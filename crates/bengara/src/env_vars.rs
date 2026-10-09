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
///
/// **前後の空白は値に残りません。** 引用符で囲んでも落とします
/// （`.env` に `A="  padded  "` と書いても、受け取るのは `padded` です）。
/// 空白そのものを値にしたいときは、設定を読む側で既定値を空白にしてください。
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
/// 戻り値は読み込んだファイルのパスと、読めなかった行の警告です。
///
/// **警告をその場では出しません。** `.env` の `APP_DEBUG` を見てからログの購読を
/// 立てる（`init_tracing`）ので、ここで `tracing::warn!` を呼ぶと誰も聞いておらず
/// 捨てられます。呼び出し側が購読を立てた**後**に流し直します。
pub(crate) fn load_dotenv(base: &Path) -> (Option<std::path::PathBuf>, Vec<String>) {
    let path = base.join(".env");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (None, Vec::new());
    };
    // Windows のメモ帳などは UTF-8 BOM を付ける。残すと1行目のキーが
    // `\u{feff}APP_NAME` になり、英数字の判定に落ちて黙って捨てられる。
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let (pairs, warnings) = parse_dotenv(text);
    for (key, value) in pairs {
        if std::env::var_os(&key).is_none() {
            // SAFETY: 起動直後、スレッドを作る前に1回だけ呼ぶ。
            // edition 2021 では set_var は安全な関数なので unsafe は余る。
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var(&key, &value)
            };
        }
    }
    (Some(path), warnings)
}

/// 行の先頭の `export` を外す。
///
/// 区切りは空白1つに限りません。`export\tAPP_NAME=x`（タブ区切り）も外します。
/// `exportAPP_NAME=x` のように続きが空白でないものは、外しません。
pub(crate) fn strip_export(line: &str) -> &str {
    match line.strip_prefix("export") {
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => line,
    }
}

/// `.env` の中身を `KEY=VALUE` の並びに変える。
///
/// - `#` から行末まではコメント（引用符で囲んだ値の中は除く）
/// - 先頭の `export` は無視する（区切りは空白かタブ）
/// - 値は `"..."` と `'...'` で囲める。`"` の中では `\n` `\r` `\t` `\\` `\"` `\'` を
///   解釈する。それ以外の `\` は `\` ごと値に残る（`"C:\data\app.sqlite"`）
///
/// 読めなかった行は飛ばしますが、黙って捨てずに警告を返します。起動時に1回しか
/// 通らないので、行番号を数える費用は気にしません。
fn parse_dotenv(text: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = strip_export(line);
        let Some((key, rest)) = line.split_once('=') else {
            warnings.push(format!(
                ".env の {number} 行目を読めませんでした（`=` がありません）"
            ));
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            warnings.push(format!(
                ".env の {number} 行目を読めませんでした（`=` の前にキーがありません）"
            ));
            continue;
        }
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            warnings.push(format!(
                ".env の {number} 行目を読めませんでした（キー `{key}` に英数字と `_` 以外が入っています）"
            ));
            continue;
        }
        out.push((
            key.to_string(),
            parse_value(rest.trim(), number, &mut warnings),
        ));
    }
    (out, warnings)
}

/// `=` の右側を値にする。`number` は警告に出す行番号。
fn parse_value(raw: &str, number: usize, warnings: &mut Vec<String>) -> String {
    let mut chars = raw.chars().peekable();
    match chars.peek() {
        Some('"') => {
            chars.next();
            let mut out = String::new();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('r') => out.push('\r'),
                        Some('t') => out.push('\t'),
                        Some('\\') => out.push('\\'),
                        Some('"') => out.push('"'),
                        Some('\'') => out.push('\''),
                        // 知らないエスケープは `\` ごと残す。落とすと
                        // `"C:\data\app.sqlite"` が `C:dataapp.sqlite` になる。
                        Some(other) => {
                            out.push('\\');
                            out.push(other);
                        }
                        None => break,
                    },
                    other => out.push(other),
                }
            }
            warn_unclosed(closed, number, warnings);
            out
        }
        Some('\'') => {
            chars.next();
            let mut out = String::new();
            let mut closed = false;
            for c in chars {
                if c == '\'' {
                    closed = true;
                    break;
                }
                out.push(c);
            }
            warn_unclosed(closed, number, warnings);
            out
        }
        // 引用符なしの値は、`#` より前までを取り、前後の空白を落とす。
        _ => strip_comment(raw).trim().to_string(),
    }
}

/// 閉じ引用符が無ければ知らせる。
///
/// 黙って行末まで値にすると、コメントまで値に混ざったまま気づけません。
fn warn_unclosed(closed: bool, number: usize, warnings: &mut Vec<String>) {
    if !closed {
        warnings.push(format!(".env の {number} 行目は引用符が閉じていません"));
    }
}

/// 値から `#` 以降を落とす。
///
/// **引用符で囲んでいない値にだけ使います。** 囲んだ値は `parse_value` が
/// 閉じ引用符までを値にするので、ここは通りません。
///
/// doc どおり「`#` から行末まではコメント」にします。`#` の前に空白を求めると
/// `APP_PORT=8000#dev` でコメントが値に混ざり、数に変換できず既定値に戻ります。
///
/// 値の途中の引用符は、引用符として数えません。数えると
/// `MAIL_FROM_NAME=Bob's App # コメント` の `#` が切れず、コメントが値に混ざります。
fn strip_comment(value: &str) -> &str {
    match value.find('#') {
        Some(at) => &value[..at],
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 警告を捨てて `KEY=VALUE` の並びだけを見る、テスト用の近道。
    fn pairs(text: &str) -> Vec<(String, String)> {
        parse_dotenv(text).0
    }

    /// 警告の一覧だけを見る、テスト用の近道。
    fn warnings(text: &str) -> Vec<String> {
        parse_dotenv(text).1
    }

    #[test]
    fn 基本的な行を読める() {
        let v = pairs("APP_NAME=myapp\nAPP_DEBUG=true\n");
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
        let v = pairs("# comment\n\nA=1 # 末尾のコメント\n");
        assert_eq!(v, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn 引用符を外す() {
        let v = pairs("A=\"hello world\"\nB='it # is'\n");
        assert_eq!(v[0].1, "hello world");
        assert_eq!(v[1].1, "it # is");
    }

    #[test]
    fn exportを無視する() {
        let v = pairs("export A=1\n");
        assert_eq!(v, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn exportの区切りはタブでもよい() {
        // 以前は半角空白1つだけを見ていたので、キーが `export\tAPP_NAME` になり
        // 英数字の判定に落ちて黙って捨てられた。
        let v = pairs("export\tAPP_NAME=myapp\nexport   B=2\n");
        assert_eq!(
            v,
            vec![
                ("APP_NAME".to_string(), "myapp".to_string()),
                ("B".to_string(), "2".to_string()),
            ]
        );
        // 続きが空白でないものは外さない（キーの一部として扱う）。
        assert_eq!(
            pairs("exportAPP_NAME=myapp\n"),
            vec![("exportAPP_NAME".to_string(), "myapp".to_string())]
        );
    }

    #[test]
    fn 知らないエスケープは円マークごと残る() {
        // Windows のパス。以前は `C:dataapp.sqlite` になっていた。
        let v = pairs("DB_DATABASE=\"C:\\data\\app.sqlite\"\n");
        assert_eq!(v[0].1, "C:\\data\\app.sqlite");
        // doc に書いてあるものは解釈する。
        let v = pairs("A=\"1\\n2\\t3\\\\4\\\"5\"\n");
        assert_eq!(v[0].1, "1\n2\t3\\4\"5");
    }

    #[test]
    fn 空白なしのコメントも切る() {
        // 以前は `#` の前に空白が要り、`8000#dev` が値になって数に変換できなかった。
        let v = pairs("APP_PORT=8000#dev\nA=1\t# タブ区切り\n");
        assert_eq!(v[0].1, "8000");
        assert_eq!(v[1].1, "1");
        // 値そのものが空になる形も通る。
        let v = pairs("A=#全部コメント\n");
        assert_eq!(v[0].1, "");
    }

    #[test]
    fn 引用符の中の井桁は値の一部() {
        let v = pairs("A=\"a#b\"\nB='c#d'\n");
        assert_eq!(v[0].1, "a#b");
        assert_eq!(v[1].1, "c#d");
    }

    #[test]
    fn 引用符で囲まない値は最初の井桁で切る() {
        // 値の途中の引用符を引用符として数えると、コメントが値に混ざる。
        let v = pairs("MAIL_FROM_NAME=Bob's App # コメント\n");
        assert_eq!(v[0].1, "Bob's App");
        // 先頭が引用符でないので、途中の `\"` も値の文字として扱い `#` で切る。
        let v = pairs("C=a\"b#c\"\n");
        assert_eq!(v[0].1, "a\"b");
    }

    #[test]
    fn 閉じていない引用符でも値は取れる() {
        // 警告は出すが、読めるところまでは読む。
        let v = pairs("A=\"閉じていない\n");
        assert_eq!(v[0].1, "閉じていない");
        let v = pairs("A='閉じていない\n");
        assert_eq!(v[0].1, "閉じていない");
    }

    #[test]
    fn 読めない行は警告の一覧に入る() {
        // 購読が立つ前に出すと捨てられるので、一覧で返して後から流し直す。
        let list = warnings("APP_KEY \"abc\"\nMY-VAR=1\n=2\nA=\"閉じていない\n");
        assert_eq!(list.len(), 4, "{list:?}");
        assert!(list[0].contains("1 行目") && list[0].contains("`=` がありません"));
        assert!(list[1].contains("2 行目") && list[1].contains("MY-VAR"));
        assert!(list[2].contains("3 行目"));
        assert!(list[3].contains("4 行目") && list[3].contains("引用符が閉じていません"));
        // 読める `.env` では警告を出さない。
        assert!(warnings("A=1\n# コメント\n").is_empty());
    }

    #[test]
    fn bomを外せば1行目も読める() {
        // メモ帳が付ける BOM。外さないと APP_NAME だけが黙って捨てられる。
        let text = "\u{feff}APP_NAME=myapp\n";
        assert!(pairs(text).is_empty(), "BOM 付きのままでは読めない");
        let stripped = text.strip_prefix('\u{feff}').unwrap();
        assert_eq!(
            pairs(stripped),
            vec![("APP_NAME".to_string(), "myapp".to_string())]
        );
    }

    #[test]
    fn 真偽値の変換() {
        assert_eq!(bool::from_env("TRUE"), Some(true));
        assert_eq!(bool::from_env("off"), Some(false));
        assert_eq!(bool::from_env("maybe"), None);
    }
}
