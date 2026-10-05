//! 多言語。画面の文字を言語ごとに差し替えます。
//!
//! ```ignore
//! __("messages.welcome")                              // "ようこそ"
//! __with("messages.greeting", &[("name", "アリス")])  // "こんにちは、アリス さん"
//! ```
//!
//! 文字は `resources/lang/*.toml` に書きます。**ビルド時に読み込む**ので、
//! 本番にファイルを配る必要はありません。

use std::future::Future;
use std::sync::{OnceLock, RwLock};

/// `bengara-build` が渡す言語の表。
///
/// `[(言語, [(鍵, 値)])]` の形です。利用者が直接作ることはありません。
pub type LangTable = &'static [(&'static str, &'static [(&'static str, &'static str)])];

static TABLE: OnceLock<LangTable> = OnceLock::new();

/// 起動時に1回だけ呼ぶ。`bengara::app!()` から渡されます。
///
/// **鍵は昇順（`str` の辞書順）に並んでいる前提**です（`Lang::lookup` が
/// 二分探索します）。並べるのは `bengara-build` の役目なので、ここでは
/// デバッグビルドのときだけ確かめます。
pub(crate) fn install(table: LangTable) {
    debug_assert!(
        table
            .iter()
            .all(|(_, entries)| entries.windows(2).all(|pair| pair[0].0 < pair[1].0)),
        "言語の表は鍵の昇順に並んでいる必要があります（bengara-build が並べます）"
    );
    let _ = TABLE.set(table);
}

fn table() -> LangTable {
    TABLE.get().copied().unwrap_or(&[])
}

/// プロセス全体で使う言語。`Lang::set` で変えられます。
fn current_lock() -> &'static RwLock<String> {
    static CURRENT: OnceLock<RwLock<String>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(crate::env("APP_LOCALE", "ja")))
}

tokio::task_local! {
    /// いまのリクエスト（タスク）だけで使う言語。
    ///
    /// `Lang::with` の中だけ入ります。リクエストごとに言語を変えても、
    /// 同時に動いているほかのリクエストに影響しません。
    static REQUEST_LOCALE: String;
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
    ///
    /// [`Lang::with`] の中なら、そのリクエストの言語を返します。
    /// 外ならプロセス全体の言語を返します。
    pub fn current() -> String {
        if let Ok(locale) = REQUEST_LOCALE.try_with(|locale| locale.clone()) {
            return locale;
        }
        current_lock()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 言語を変える。
    ///
    /// **プロセス全体に効きます。リクエストごとに変えるときは [`Lang::with`] を
    /// 使ってください。** ここで変えると、同時に処理している別のリクエストの
    /// `__()` の結果も変わります。
    ///
    /// 起動時に1回だけ決める用途（`APP_LOCALE` の上書きなど）に向いています。
    pub fn set(locale: impl Into<String>) {
        let mut current = current_lock().write().unwrap_or_else(|e| e.into_inner());
        *current = locale.into();
    }

    /// その処理の間だけ言語を変える。
    ///
    /// ```ignore
    /// // ?locale=en のときだけ英語で返す
    /// Lang::with(locale, async move {
    ///     json(&serde_json::json!({ "message": __("messages.welcome") }))
    /// })
    /// .await
    /// ```
    ///
    /// 中で待っている間に別のリクエストが入っても、互いに影響しません。
    pub async fn with<F: Future>(locale: impl Into<String>, future: F) -> F::Output {
        REQUEST_LOCALE.scope(locale.into(), future).await
    }

    /// 読み込まれている言語の一覧。
    pub fn available() -> Vec<&'static str> {
        // 言語の数は少ないので、ここは順に見るだけで十分です。
        table().iter().map(|(name, _)| *name).collect()
    }

    /// その言語があるか。
    pub fn has(locale: &str) -> bool {
        table().iter().any(|(name, _)| *name == locale)
    }

    /// 鍵を引く。無ければ `None`。
    fn lookup(locale: &str, key: &str) -> Option<&'static str> {
        let (_, entries) = table().iter().find(|(name, _)| *name == locale)?;
        // **鍵は昇順（`str` の辞書順）に並んでいる前提**です。`bengara-build` が
        // 並べて渡します。並んでいないと引けない鍵が出ます。
        entries
            .binary_search_by_key(&key, |(entry_key, _)| *entry_key)
            .ok()
            .map(|index| entries[index].1)
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
    // **長い鍵から置き換えます。** 渡された順だと、`:name` が `:name_kanji` の
    // 前に当たって `アリス_kanji` のような結果になります。
    let mut ordered = values.to_vec();
    ordered.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    for (name, value) in ordered {
        text = text.replace(&format!(":{name}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **鍵は昇順に並べます**（`lookup` が二分探索するため）。
    const JA: &[(&str, &str)] = &[
        ("messages.greeting", "こんにちは、:name さん"),
        ("messages.name_both", ":name（:name_kanji）"),
        ("messages.welcome", "ようこそ"),
    ];
    const EN: &[(&str, &str)] = &[("messages.welcome", "Welcome")];
    const TEST_TABLE: LangTable = &[("ja", JA), ("en", EN)];

    /// テーブルは1回しか入れられないので、入っていなければ入れる。
    fn ensure_table() {
        let _ = TABLE.set(TEST_TABLE);
    }

    #[test]
    fn 表は鍵の昇順に並んでいる() {
        // `lookup` の二分探索はこの並びを前提にしています。
        for (_, entries) in TEST_TABLE {
            assert!(
                entries.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "鍵が昇順に並んでいません"
            );
        }
    }

    #[tokio::test]
    async fn 鍵から文字を引ける() {
        ensure_table();
        assert_eq!(
            Lang::with("ja", async { __("messages.welcome") }).await,
            "ようこそ"
        );
        assert_eq!(
            Lang::with("en", async { __("messages.welcome") }).await,
            "Welcome"
        );
    }

    #[tokio::test]
    async fn 言語はリクエストごとに変わる() {
        ensure_table();
        Lang::set("ja");

        let inner = Lang::with("en", async {
            // 中では en。
            assert_eq!(Lang::current(), "en");
            // さらに入れ子にしても、内側だけが変わる。
            let deep = Lang::with("ja", async { Lang::current() }).await;
            assert_eq!(deep, "ja");
            assert_eq!(Lang::current(), "en", "入れ子から戻る");
            __("messages.welcome")
        })
        .await;

        assert_eq!(inner, "Welcome");
        // 外は変わっていない。
        assert_eq!(Lang::current(), "ja");
        assert_eq!(__("messages.welcome"), "ようこそ");
    }

    #[tokio::test]
    async fn 無い鍵は鍵をそのまま返す() {
        ensure_table();
        assert_eq!(
            Lang::with("ja", async { __("messages.none") }).await,
            "messages.none"
        );
        // 鍵が表の端より前・後にあっても落ちない（二分探索の端の確認）。
        assert_eq!(Lang::with("ja", async { __("aaa") }).await, "aaa");
        assert_eq!(Lang::with("ja", async { __("zzz") }).await, "zzz");
    }

    #[tokio::test]
    async fn 差し替えができる() {
        ensure_table();
        let greeting = Lang::with("ja", async {
            __with("messages.greeting", &[("name", "アリス")])
        })
        .await;
        assert_eq!(greeting, "こんにちは、アリス さん");

        // 無い名前は何も起きない。
        let welcome = Lang::with("ja", async {
            __with("messages.welcome", &[("name", "アリス")])
        })
        .await;
        assert_eq!(welcome, "ようこそ");
    }

    #[tokio::test]
    async fn 前方一致する鍵も正しく置き換わる() {
        ensure_table();
        // `:name` を先に当てると `アリス_kanji` になってしまう。
        let text = Lang::with("ja", async {
            __with(
                "messages.name_both",
                &[("name", "アリス"), ("name_kanji", "有栖")],
            )
        })
        .await;
        assert_eq!(text, "アリス（有栖）");

        // 渡す順を変えても同じ結果になる。
        let text = Lang::with("ja", async {
            __with(
                "messages.name_both",
                &[("name_kanji", "有栖"), ("name", "アリス")],
            )
        })
        .await;
        assert_eq!(text, "アリス（有栖）");
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

    #[tokio::test]
    async fn 無い言語では既定の言語を見る() {
        ensure_table();
        // fr は無いので、APP_FALLBACK_LOCALE（既定 ja）を見る。
        assert_eq!(
            Lang::with("fr", async { __("messages.welcome") }).await,
            "ようこそ"
        );
    }

    #[test]
    fn プロセス全体の言語も変えられる() {
        ensure_table();
        Lang::set("ja");
        assert_eq!(Lang::current(), "ja");
    }
}
