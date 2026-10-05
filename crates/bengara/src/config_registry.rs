//! 設定の保管場所。
//!
//! `config/` の各ファイルの `config()` を起動時に1回ずつ呼び、戻り値を型ごとに保管します。
//! 以後は読むだけなので、ロックは要りません。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::env_vars::env;

/// 型ごとに設定を1つ保管する入れ物。
#[derive(Default)]
pub struct Registry {
    values: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Registry {
    /// 空の入れ物を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 設定を登録する。同じ型を2回登録すると、後のものが残る。
    pub fn set<T: Any + Send + Sync>(&mut self, value: T) {
        self.values.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// 登録済みの設定を借りる。
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.values.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }

    /// 登録されている設定の数。
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// 何も登録されていないか。
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("len", &self.values.len())
            .finish()
    }
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// 起動時に1回だけ呼ぶ。2回目以降は何もしない。
pub(crate) fn install(registry: Registry) {
    let _ = REGISTRY.set(registry);
}

/// すでに設定が入っているか。
pub(crate) fn installed() -> bool {
    REGISTRY.get().is_some()
}

/// 設定を型で読み出す。
///
/// ```ignore
/// let name = &config::<AppConfig>().name;
/// ```
///
/// # パニック
///
/// その型の設定が登録されていないときはパニックします。`config/` に対応する
/// ファイルを置いて、`pub fn config() -> その型` を定義してください。
pub fn config<T: Any + Send + Sync>() -> &'static T {
    try_config::<T>().unwrap_or_else(|| {
        panic!(
            "設定 {} が登録されていません。config/ の中に `pub fn config() -> {}` を返すファイルを置いてください",
            std::any::type_name::<T>(),
            std::any::type_name::<T>()
        )
    })
}

/// 設定を型で読み出す。登録されていなければ `None`。
pub fn try_config<T: Any + Send + Sync>() -> Option<&'static T> {
    REGISTRY.get()?.get::<T>()
}

/// `config/app.rs` が返す基本設定。Laravel の `config/app.php` に当たります。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// アプリ名。
    pub name: String,
    /// 実行環境（`local`、`production` など）。
    pub env: String,
    /// 詳しいエラーページを出すか。
    pub debug: bool,
    /// アプリの URL。
    pub url: String,
    /// 署名に使う鍵（`APP_KEY`）。
    ///
    /// セッションと署名付き URL で使います。**空のままだと、それらを使うときにエラーになります。**
    /// `cargo artisan key:generate` で作ってください。
    pub key: String,
}

impl Default for AppConfig {
    /// `config/app.rs` が無いときに使われる値。環境変数から組み立てます。
    fn default() -> Self {
        Self {
            name: env("APP_NAME", "bengara"),
            env: env("APP_ENV", "production"),
            debug: env("APP_DEBUG", false),
            url: env("APP_URL", "http://localhost"),
            key: env("APP_KEY", ""),
        }
    }
}

impl AppConfig {
    /// 本番環境か。
    pub fn is_production(&self) -> bool {
        self.env == "production"
    }

    /// 手元の開発環境か。
    pub fn is_local(&self) -> bool {
        self.env == "local"
    }

    /// 署名に使う鍵を取り出す。設定されていなければ、直し方を書いたエラーを返す。
    pub(crate) fn signing_key(&self) -> crate::error::Result<&[u8]> {
        if self.key.is_empty() {
            return Err(crate::Error::msg(
                "APP_KEY が設定されていません。`cargo artisan key:generate` で作って .env に入れてください",
            ));
        }
        Ok(self.key.as_bytes())
    }
}

/// 基本設定を取り出す。`config/app.rs` が無ければ既定値を使います。
pub(crate) fn app_config() -> &'static AppConfig {
    static FALLBACK: OnceLock<AppConfig> = OnceLock::new();
    try_config::<AppConfig>().unwrap_or_else(|| FALLBACK.get_or_init(AppConfig::default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Sample(u32);

    #[test]
    fn 型ごとに出し入れできる() {
        let mut r = Registry::new();
        assert!(r.is_empty());
        r.set(Sample(1));
        assert_eq!(r.get::<Sample>(), Some(&Sample(1)));
        r.set(Sample(2));
        assert_eq!(r.get::<Sample>(), Some(&Sample(2)));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn 未登録の型は見つからない() {
        let r = Registry::new();
        assert_eq!(r.get::<Sample>(), None);
    }
}
