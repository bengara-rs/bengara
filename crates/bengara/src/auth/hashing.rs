//! パスワードの変換（ハッシュ）。
//!
//! 方式は **PBKDF2-HMAC-SHA256**（RFC 8018）です。土台は `support::crypto` の
//! HMAC-SHA256 なので、依存クレートは増えません（決定記録 #046）。
//!
//! ```ignore
//! let stored = Hash::make("ひみつ");
//! assert!(Hash::check("ひみつ", &stored));
//! ```

use std::sync::OnceLock;

use crate::error::Result;
use crate::support::crypto;

/// 既定の繰り返し回数。
///
/// 実測（リリースビルド）で 1 回の照合が約 0.23 秒になる回数です。
/// 速すぎると総当たりに弱く、遅すぎるとログインが重くなります。
pub(crate) const DEFAULT_ITERATIONS: u32 = 120_000;

/// テストのときに使う回数。
///
/// デバッグビルドのテストで 120,000 回を回すと 1 件あたり 3.6 秒かかるので、
/// テストでは下げます。`HASH_ITERATIONS` を指定すればその値が優先されます。
pub(crate) const TEST_ITERATIONS: u32 = 1_000;

/// 塩の長さ（バイト）。
const SALT_BYTES: usize = 16;

/// 保存する文字列の先頭に付ける方式の名前。
const SCHEME: &str = "pbkdf2-sha256";

/// 保存されている値の `i=` に許す上限。
///
/// 照合は保存されている回数で計算します。その値をそのまま信じると、
/// DB に細工した値を入れられる経路があったときに、1 回の照合で計算を
/// いくらでも引き延ばせます（既定の 120,000 回で 0.23 秒なので、
/// 1,000 万回なら 20 秒ほど）。上限を超える値は照合せずに偽とします。
const MAX_ITERATIONS: u32 = 10_000_000;

/// ハッシュの設定。`config/hashing.rs` が返します。
///
/// ```ignore
/// pub fn config() -> HashConfig {
///     HashConfig { iterations: env("HASH_ITERATIONS", 120_000u32) }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashConfig {
    /// PBKDF2 の繰り返し回数。大きいほど安全で、そのぶん遅くなります。
    pub iterations: u32,
}

impl Default for HashConfig {
    /// `config/hashing.rs` が無いときに使われる値。
    fn default() -> Self {
        Self {
            iterations: crate::env("HASH_ITERATIONS", DEFAULT_ITERATIONS),
        }
    }
}

/// テストのときに使う回数の上書き。`testing` から入れます。
static TEST_OVERRIDE: OnceLock<u32> = OnceLock::new();

/// テスト用に回数を下げる。`#[bengara::test]` が 1 回だけ呼びます。
///
/// **`.env` を読む前**に呼ばれるので、見るのは本物の環境変数だけです。
/// `HASH_ITERATIONS=50000 cargo test` のように指定したときだけ、その値を使います。
/// `.env` に書いた値（開発用に下げた値など）はテストに影響しません。
pub(crate) fn install_test_iterations() {
    if std::env::var("HASH_ITERATIONS").is_ok() {
        return;
    }
    let _ = TEST_OVERRIDE.set(TEST_ITERATIONS);
}

/// いま使う回数。
pub(crate) fn iterations() -> u32 {
    if let Some(found) = TEST_OVERRIDE.get() {
        return *found;
    }
    static FALLBACK: OnceLock<HashConfig> = OnceLock::new();
    crate::try_config::<HashConfig>()
        .unwrap_or_else(|| FALLBACK.get_or_init(HashConfig::default))
        .iterations
        .max(1)
}

/// パスワードの変換。Laravel の `Hash` に当たります。
///
/// **`make` と `check` は時間がかかります**（既定で 0.2 秒ほど）。
/// 非同期の処理（ハンドラ・ジョブ・コマンドの `async fn`）の中では
/// [`make_async`](Hash::make_async) / [`check_async`](Hash::check_async) を使ってください。
/// `make` / `check` をそのまま呼ぶと、その 0.2 秒のあいだ tokio のワーカースレッドを
/// 塞ぎ、同じスレッドで動くほかのリクエストが止まります。
///
/// `req.auth().attempt(...)` は中で裏のスレッドに逃がしているので、そのまま使えます。
pub struct Hash;

impl Hash {
    /// 保存する形に変える。塩は毎回変わるので、**同じパスワードでも結果は毎回違います。**
    ///
    /// **非同期の処理の中では [`make_async`](Hash::make_async) を使ってください。**
    /// ここでは 0.2 秒ほどスレッドを塞ぎます。
    pub fn make(password: &str) -> String {
        Self::make_with(password, iterations())
    }

    /// `make` を裏のスレッドで行う。**非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// let password = Hash::make_async(&input).await?;
    /// ```
    pub async fn make_async(password: &str) -> Result<String> {
        let password = password.to_string();
        crate::auth::blocking(move || Self::make(&password)).await
    }

    /// 回数を指定して変える。
    pub fn make_with(password: &str, iterations: u32) -> String {
        let iterations = iterations.max(1);
        let salt = crypto::random_bytes::<SALT_BYTES>();
        let digest = crypto::pbkdf2_sha256(password.as_bytes(), &salt, iterations);
        format!(
            "${SCHEME}$i={iterations}${}${}",
            crypto::to_hex(&salt),
            crypto::to_hex(&digest)
        )
    }

    /// 平文と、保存してある値を照合する。
    ///
    /// 値の形が壊れているときは偽を返します（エラーにはしません）。
    ///
    /// **非同期の処理の中では [`check_async`](Hash::check_async) を使ってください。**
    /// ここでは 0.2 秒ほどスレッドを塞ぎます。
    pub fn check(password: &str, hashed: &str) -> bool {
        let Some(parsed) = Parsed::parse(hashed) else {
            tracing::warn!("保存されているパスワードの形が正しくありません");
            return false;
        };
        // 回数は保存されている値から取るので、上限を決めておく。
        if parsed.iterations > MAX_ITERATIONS {
            tracing::warn!(
                "保存されているパスワードの繰り返し回数が多すぎます（{} 回、上限は {MAX_ITERATIONS} 回）",
                parsed.iterations
            );
            return false;
        }
        let digest = crypto::pbkdf2_sha256(password.as_bytes(), &parsed.salt, parsed.iterations);
        crypto::constant_time_eq(&digest, &parsed.digest)
    }

    /// `check` を裏のスレッドで行う。**非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// if Hash::check_async(&input, user.password_hash()).await? { ... }
    /// ```
    pub async fn check_async(password: &str, hashed: &str) -> Result<bool> {
        let password = password.to_string();
        let hashed = hashed.to_string();
        crate::auth::blocking(move || Self::check(&password, &hashed)).await
    }

    /// 保存してある値を作り直したほうがよいか。
    ///
    /// 回数の設定を上げたあと、ログインのついでに作り直すのに使います。
    pub fn needs_rehash(hashed: &str) -> bool {
        match Parsed::parse(hashed) {
            Some(parsed) => parsed.iterations < iterations(),
            // 読めない値は作り直す対象とする。
            None => true,
        }
    }

    /// 照合にかかる時間を合わせるための空回し。
    ///
    /// 利用者が見つからなかったときに呼びます。何もしないで帰ると、
    /// 「そのメールアドレスは登録されていない」ことが応答の速さから分かってしまいます。
    ///
    /// 比べる相手は**計算せずに組み立てます**。`Hash::make` で作ると、
    /// プロセス起動後の最初の1件だけ PBKDF2 が 2 回走り、そこだけ約 2 倍遅くなります。
    /// それでは「登録されていない1件目」が見分けられてしまいます。
    /// 照合は必ず外れますが、かかる時間は本物の照合と同じ（PBKDF2 を 1 回）です。
    pub(crate) fn waste_time() {
        let _ = Hash::check("bengara-dummy-password", &dummy_hash(iterations()));
    }
}

/// `waste_time` が照合する相手。合うことはありません。
///
/// 塩とハッシュはすべて 0 の固定値です。中身が当たっても意味がないので、
/// 推測できなくする必要はありません。
fn dummy_hash(iterations: u32) -> String {
    format!(
        "${SCHEME}$i={iterations}${}${}",
        "00".repeat(SALT_BYTES),
        "00".repeat(32)
    )
}

/// 保存してある文字列を分解した結果。
struct Parsed {
    iterations: u32,
    salt: Vec<u8>,
    digest: Vec<u8>,
}

impl Parsed {
    /// `$pbkdf2-sha256$i=120000$<塩>$<ハッシュ>` を分解する。
    fn parse(hashed: &str) -> Option<Self> {
        let mut parts = hashed.split('$');
        // 先頭は `$` なので空文字になる。
        if parts.next() != Some("") {
            return None;
        }
        if parts.next()? != SCHEME {
            return None;
        }
        let iterations = parts.next()?.strip_prefix("i=")?.parse().ok()?;
        let salt = crypto::from_hex(parts.next()?)?;
        let digest = crypto::from_hex(parts.next()?)?;
        if parts.next().is_some() || salt.is_empty() || digest.len() != 32 {
            return None;
        }
        Some(Self {
            iterations,
            salt,
            digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストでは回数を下げる（1 件ごとに 3 秒かかるのを避ける）。
    const FAST: u32 = 50;

    #[test]
    fn 作った値で照合できる() {
        let stored = Hash::make_with("ひみつの言葉", FAST);
        assert!(Hash::check("ひみつの言葉", &stored));
        assert!(!Hash::check("ちがう言葉", &stored));
        assert!(!Hash::check("", &stored));
    }

    #[test]
    fn 同じパスワードでも毎回違う値になる() {
        let a = Hash::make_with("same", FAST);
        let b = Hash::make_with("same", FAST);
        assert_ne!(a, b, "塩が毎回変わる");
        assert!(Hash::check("same", &a));
        assert!(Hash::check("same", &b));
    }

    #[test]
    fn 保存する形に方式と回数が入っている() {
        let stored = Hash::make_with("x", 123);
        assert!(stored.starts_with("$pbkdf2-sha256$i=123$"), "{stored}");
        // 方式 + 回数 + 塩(32) + ハッシュ(64) + 区切り
        assert_eq!(stored.matches('$').count(), 4);
    }

    #[test]
    fn 回数が違っても照合できる() {
        let stored = Hash::make_with("x", 7);
        assert!(Hash::check("x", &stored), "保存した値の回数を使う");
    }

    #[test]
    fn 壊れた値は偽になる() {
        for broken in [
            "",
            "ただの文字列",
            "$pbkdf2-sha256$i=10$zz$zz",
            "$pbkdf2-sha256$i=abc$00$00",
            "$argon2$i=10$00$00",
            "$pbkdf2-sha256$i=10$00",
        ] {
            assert!(!Hash::check("x", broken), "{broken}");
            assert!(Hash::needs_rehash(broken), "{broken}");
        }
    }

    #[test]
    fn 回数が足りなければ作り直す対象になる() {
        let few = Hash::make_with("x", 1);
        assert!(Hash::needs_rehash(&few), "設定より少ない");

        // 多い側は**計算しない**（u32::MAX 回を実際に回すと終わらない）。
        // 保存されている形だけを組み立てて判定させる。
        let many = format!("$pbkdf2-sha256$i={}$00${}", u32::MAX, "ab".repeat(32));
        assert!(!Hash::needs_rehash(&many), "設定より多い");
    }

    #[test]
    fn 既定の回数は実測に合わせた値() {
        assert_eq!(DEFAULT_ITERATIONS, 120_000);
        assert_eq!(HashConfig::default().iterations, DEFAULT_ITERATIONS);
    }

    #[test]
    fn 回数が上限を超えていれば計算せずに偽になる() {
        // 1 回も PBKDF2 を回さないこと（回すと終わらない）を、時間でも確かめる。
        let huge = format!(
            "$pbkdf2-sha256$i={}$00${}",
            MAX_ITERATIONS + 1,
            "ab".repeat(32)
        );
        let started = std::time::Instant::now();
        assert!(!Hash::check("x", &huge));
        assert!(started.elapsed().as_secs() < 1, "すぐ返る");

        // ちょうど上限なら断らない（形が合っていれば照合する）。
        let limit = format!("$pbkdf2-sha256$i={MAX_ITERATIONS}$00${}", "ab".repeat(32));
        assert!(Parsed::parse(&limit).is_some(), "形としては読める");
    }

    #[test]
    fn 空回しは毎回同じだけ計算する() {
        // `Hash::make` で相手を作っていたころは、初回だけ PBKDF2 が 2 回走った。
        // いまは組み立てるだけなので、1 回目と 2 回目で差が出ない。
        let dummy = dummy_hash(FAST);
        assert!(Parsed::parse(&dummy).is_some(), "照合できる形になっている");
        assert!(!Hash::check("bengara-dummy-password", &dummy), "必ず外れる");

        // 回数は設定どおりで、本物の照合と同じだけかかる。
        assert_eq!(Parsed::parse(&dummy_hash(123)).unwrap().iterations, 123);
        assert_eq!(Parsed::parse(&dummy).unwrap().salt.len(), SALT_BYTES);

        // 2 回続けて呼んでも落ちない（回数は下げておく）。
        install_test_iterations();
        Hash::waste_time();
        Hash::waste_time();
    }

    #[tokio::test]
    async fn 非同期版でも往復できる() {
        // 既定の 120,000 回はデバッグビルドで 1 件 3.6 秒かかるので下げる。
        install_test_iterations();

        let stored = Hash::make_async("ひみつの言葉").await.unwrap();
        assert!(Hash::check_async("ひみつの言葉", &stored).await.unwrap());
        assert!(!Hash::check_async("ちがう言葉", &stored).await.unwrap());
    }
}
