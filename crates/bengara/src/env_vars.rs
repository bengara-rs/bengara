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
/// **空の環境変数は「設定されていない」と見なします。** 前後の空白を落として空に
/// なるものも含みます。`env()` が空を未設定として扱うのと同じ規則です。
/// docker-compose の `APP_KEY=${APP_KEY}` は、ホスト側で未設定だと空の変数を
/// 作ります。これで `.env` の値が黙って隠れないようにするためです。
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
    // `${VAR}` は「本物の環境変数 > `.env` の前の行」で展開する。
    let real = |key: &str| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    let (pairs, warnings) = parse_dotenv(text, &real);
    for (key, value) in pairs {
        if is_unset(std::env::var_os(&key).as_deref()) {
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

/// 実際の環境変数が「未設定」かどうか。無いか、空白だけなら未設定。
///
/// UTF-8 でない値は中身があるものとして扱います（上書きしない）。
pub(crate) fn is_unset(value: Option<&std::ffi::OsStr>) -> bool {
    match value {
        None => true,
        Some(raw) => raw.to_str().is_some_and(|text| text.trim().is_empty()),
    }
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
///
/// # `${VAR}` の展開
///
/// Laravel（phpdotenv）と同じく、**引用符なしと `"..."` の値**の中の `${VAR}` を
/// 展開します。`'...'` の中は展開しません。
///
/// - 値は「本物の環境変数（`real`）> この `.env` の前の行」の順で探します。
///   後の行に書いた変数は使えません。
/// - どちらにも無い（か空の）変数は空文字になります。
/// - `$VAR`（波かっこなし）は展開しません。閉じていない `${` はそのまま残します。
///
/// ```text
/// APP_NAME=bengara
/// MAIL_FROM_NAME="${APP_NAME}"   # → bengara
/// RAW='${APP_NAME}'              # → ${APP_NAME}
/// ```
///
/// # `#` を含む値は必ず引用符で囲んでください
///
/// 引用符で囲んでいない値は、**最初の `#` より前までが値になります。**
/// `DB_PASSWORD=p#ss` は `p` になり、警告も出ません。
/// `#` を値に入れたいときは囲みます（`DB_PASSWORD="p#ss"`）。
/// 囲んだ値の中の `#` は、そのまま値に残ります。
fn parse_dotenv(
    text: &str,
    real: &dyn Fn(&str) -> Option<String>,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut out: Vec<(String, String)> = Vec::new();
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
        let (value, expandable) = parse_raw(rest.trim(), number, &mut warnings);
        let value = if expandable {
            expand(&value, |name| {
                real(name).or_else(|| {
                    // 同じキーが 2 回あれば、読み込みと同じく先の行を採る。
                    out.iter()
                        .find(|(earlier, _)| earlier == name)
                        .map(|(_, value)| value.clone())
                })
            })
        } else {
            value
        };
        out.push((key.to_string(), value));
    }
    (out, warnings)
}

/// `.env` の `=` の右側を、読み込みと同じ規則で値にする。
///
/// `key:generate` が「`APP_KEY` は設定済みか」を見るのに使います。別々に
/// 読むと、`APP_KEY=""` や `APP_KEY= # 後で` を「設定済み」と見てしまいます
/// （読み込みではどちらも空です）。警告は読み込みのときに出るので、ここでは捨てます。
///
/// `${VAR}` は本物の環境変数だけで展開します（前の行までは見ません）。
pub(crate) fn dotenv_value(raw: &str) -> String {
    let (value, expandable) = parse_raw(raw.trim(), 0, &mut Vec::new());
    if expandable {
        expand(&value, |name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    } else {
        value
    }
}

/// 値の中の `${VAR}` を `resolve` の結果に置き換える。
///
/// 見つからない変数は空文字にします（phpdotenv と同じ）。名前に英数字と `_`
/// 以外が入っているもの・閉じていない `${` は、そのまま残します。
fn expand(value: &str, resolve: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let name = after.find('}').map(|end| &after[..end]).filter(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        match name {
            Some(name) => {
                out.push_str(&resolve(name).unwrap_or_default());
                rest = &after[name.len() + 1..];
            }
            None => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `=` の右側を値にする。`number` は警告に出す行番号。
///
/// 2 つ目は `${VAR}` を展開してよいか（引用符なしと `"..."` は真、`'...'` は偽）。
fn parse_raw(raw: &str, number: usize, warnings: &mut Vec<String>) -> (String, bool) {
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
            (out, true)
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
            (out, false)
        }
        // 引用符なしの値は、`#` より前までを取り、前後の空白を落とす。
        _ => (strip_comment(raw).trim().to_string(), true),
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
/// **引用符で囲んでいない値にだけ使います。** 囲んだ値は `parse_raw` が
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
        parse_dotenv(text, &|_| None).0
    }

    /// 警告の一覧だけを見る、テスト用の近道。
    fn warnings(text: &str) -> Vec<String> {
        parse_dotenv(text, &|_| None).1
    }

    #[test]
    fn 波かっこの変数を前の行の値で展開する() {
        let v = pairs("APP_NAME=bengara\nMAIL_FROM_NAME=\"${APP_NAME}\"\nB=${APP_NAME}-app\n");
        assert_eq!(v[1].1, "bengara");
        assert_eq!(v[2].1, "bengara-app");
    }

    #[test]
    fn initの雛形は警告なしに読めて差出人の名前はアプリ名になる() {
        // `init` が作る `.env`。`MAIL_FROM_NAME="${APP_NAME}"`（Laravel と同じ書き方）が効くこと。
        let text = include_str!("cli/stubs/env.stub").replace("{{name}}", "myapp");
        let (v, warnings) = parse_dotenv(&text, &|_| None);
        assert!(warnings.is_empty(), "{warnings:?}");
        let get = |key: &str| v.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(get("MAIL_FROM_NAME"), Some("myapp"));
        assert_eq!(get("MAIL_FROM_ADDRESS"), Some("noreply@example.com"));
        assert_eq!(get("APP_KEY"), Some(""));
    }

    #[test]
    fn 単一引用符の中は展開しない() {
        let v = pairs("A=x\nB='${A}'\n");
        assert_eq!(v[1].1, "${A}");
    }

    #[test]
    fn 本物の環境変数が前の行より優先される() {
        let real = |key: &str| (key == "APP_NAME").then(|| "real".to_string());
        let (v, _) = parse_dotenv("APP_NAME=dotenv\nA=\"${APP_NAME}\"\n", &real);
        assert_eq!(v[1].1, "real");
    }

    #[test]
    fn 展開できないものの扱い() {
        // 後の行・どこにも無い変数は空になる。
        let v = pairs("A=[${B}]\nB=1\n");
        assert_eq!(v[0].1, "[]");
        // 同じキーが 2 回あれば先の行を使う（読み込みと同じ）。
        let v = pairs("A=1\nA=2\nB=${A}\n");
        assert_eq!(v[2].1, "1");
        // `$VAR`・閉じていない `${`・名前に使えない文字はそのまま残す。
        let v = pairs("A=1\nB=$A\nC=\"${A\"\nD=\"${A-B}\"\nE=\"${}\"\n");
        assert_eq!(v[1].1, "$A");
        assert_eq!(v[2].1, "${A");
        assert_eq!(v[3].1, "${A-B}");
        assert_eq!(v[4].1, "${}");
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
    fn 右側だけを読み込みと同じ規則で値にする() {
        // `key:generate` が使う。読み込みと 1 つの規則を共有していること。
        for raw in [
            "abc",
            "  abc  ",
            "\"\"",
            "''",
            " # 後で",
            "#全部コメント",
            "\"a#b\"",
            "abc # コメント",
            "\"閉じていない",
            "",
        ] {
            let loaded = pairs(&format!("A={raw}\n"));
            assert_eq!(dotenv_value(raw), loaded[0].1, "{raw}");
        }
        assert_eq!(dotenv_value("\"\""), "");
        assert_eq!(dotenv_value(" # 後で"), "");
        assert_eq!(dotenv_value("abc # コメント"), "abc");
    }

    #[test]
    fn 空の環境変数は未設定と見なして埋める() {
        use std::ffi::OsStr;
        assert!(is_unset(None));
        assert!(is_unset(Some(OsStr::new(""))));
        assert!(is_unset(Some(OsStr::new("  \t"))));
        assert!(!is_unset(Some(OsStr::new("base64:abc"))));
        assert!(!is_unset(Some(OsStr::new(" x "))));
    }

    #[test]
    fn 真偽値の変換() {
        assert_eq!(bool::from_env("TRUE"), Some(true));
        assert_eq!(bool::from_env("off"), Some(false));
        assert_eq!(bool::from_env("maybe"), None);
    }
}
