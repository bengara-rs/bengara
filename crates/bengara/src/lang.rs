//! 多言語。画面の文字を言語ごとに差し替えます。
//!
//! ```ignore
//! __("messages.welcome")                              // "ようこそ"
//! __with("messages.greeting", &[("name", "アリス")])  // "こんにちは、アリス さん"
//! ```
//!
//! 文字は `resources/lang/*.toml` に書きます。**ビルド時に読み込む**ので、
//! 本番にファイルを配る必要はありません。

use std::sync::{OnceLock, RwLock};

/// `bengara-build` が渡す言語の表。
///
/// `[(言語, [(鍵, 値)])]` の形です。利用者が直接作ることはありません。
pub type LangTable = &'static [(&'static str, &'static [(&'static str, &'static str)])];

static TABLE: OnceLock<LangTable> = OnceLock::new();

/// 起動時に1回だけ呼ぶ。`bengara::app!()` から渡されます。
pub(crate) fn install(table: LangTable) {
    let _ = TABLE.set(table);
}

fn table() -> LangTable {
    TABLE.get().copied().unwrap_or(&[])
}

/// いま使う言語。`Lang::set` で変えられます。
fn current_lock() -> &'static RwLock<String> {
    static CURRENT: OnceLock<RwLock<String>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(crate::env("APP_LOCALE", "ja")))
}

/// 見つからないときに見る言語。
fn fallback() -> String {
    static FALLBACK: OnceLock<String> = OnceLock::new();
    FALLBACK
        .get_or_init(|| crate::env("APP_FALLBACK_LOCALE", "ja"))
        .clone()
}

/// 言語の切り替え。
pub struct Lang;

impl Lang {
    /// いま使っている言語。
    pub fn current() -> String {
        current_lock()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 言語を変える。
    ///
    /// **プロセス全体に効きます。** リクエストごとに変えたいときは、
    /// ミドルウェアで毎回設定してください。
    pub fn set(locale: impl Into<String>) {
        let mut current = current_lock().write().unwrap_or_else(|e| e.into_inner());
        *current = locale.into();
    }

    /// 読み込まれている言語の一覧。
    pub fn available() -> Vec<&'static str> {
        table().iter().map(|(name, _)| *name).collect()
    }

    /// その言語があるか。
    pub fn has(locale: &str) -> bool {
        table().iter().any(|(name, _)| *name == locale)
    }

    /// 鍵を引く。無ければ `None`。
    fn lookup(locale: &str, key: &str) -> Option<&'static str> {
        let (_, entries) = table().iter().find(|(name, _)| *name == locale)?;
        entries
            .iter()
            .find(|(entry_key, _)| *entry_key == key)
            .map(|(_, value)| *value)
    }
}

/// 鍵から文字を引く。Laravel の `__()` に当たります。
///
/// 見つからない鍵は**鍵をそのまま返します**（画面が壊れないように）。
pub fn __(key: &str) -> String {
    let current = Lang::current();
    if let Some(found) = Lang::lookup(&current, key) {
        return found.to_string();
    }
    let fallback = fallback();
    if fallback != current {
        if let Some(found) = Lang::lookup(&fallback, key) {
            return found.to_string();
        }
    }
    tracing::warn!("言語の鍵 `{key}` が見つかりません（言語: {current}）");
    key.to_string()
}

/// 鍵から文字を引き、`:name` の部分を差し替える。
///
/// ```ignore
/// // greeting = "こんにちは、:name さん"
/// __with("messages.greeting", &[("name", "アリス")]);   // "こんにちは、アリス さん"
/// ```
pub fn __with(key: &str, values: &[(&str, &str)]) -> String {
    let mut text = __(key);
    for (name, value) in values {
        text = text.replace(&format!(":{name}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const JA: &[(&str, &str)] = &[
        ("messages.welcome", "ようこそ"),
        ("messages.greeting", "こんにちは、:name さん"),
    ];
    const EN: &[(&str, &str)] = &[("messages.welcome", "Welcome")];
    const TEST_TABLE: LangTable = &[("ja", JA), ("en", EN)];

    /// テーブルは1回しか入れられないので、入っていなければ入れる。
    fn ensure_table() {
        let _ = TABLE.set(TEST_TABLE);
    }

    #[test]
    fn 鍵から文字を引ける() {
        ensure_table();
        Lang::set("ja");
        assert_eq!(__("messages.welcome"), "ようこそ");

        Lang::set("en");
        assert_eq!(__("messages.welcome"), "Welcome");
        // 戻す（ほかのテストに影響しないように）。
        Lang::set("ja");
    }

    #[test]
    fn 無い鍵は鍵をそのまま返す() {
        ensure_table();
        assert_eq!(__("messages.none"), "messages.none");
    }

    #[test]
    fn 差し替えができる() {
        ensure_table();
        Lang::set("ja");
        assert_eq!(
            __with("messages.greeting", &[("name", "アリス")]),
            "こんにちは、アリス さん"
        );
        // 無い名前は何も起きない。
        assert_eq!(
            __with("messages.welcome", &[("name", "アリス")]),
            "ようこそ"
        );
    }

    #[test]
    fn 読み込まれている言語が分かる() {
        ensure_table();
        let mut available = Lang::available();
        available.sort();
        assert_eq!(available, vec!["en", "ja"]);
        assert!(Lang::has("ja"));
        assert!(!Lang::has("fr"));
    }

    #[test]
    fn 無い言語では既定の言語を見る() {
        ensure_table();
        Lang::set("fr");
        // fr は無いので、APP_FALLBACK_LOCALE（既定 ja）を見る。
        assert_eq!(__("messages.welcome"), "ようこそ");
        Lang::set("ja");
    }
}
