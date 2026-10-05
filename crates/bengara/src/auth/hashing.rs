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
/// 非同期の処理の中で直接呼ぶと、その間ほかのリクエストを待たせます。
/// `req.auth().attempt(...)` は裏のスレッドに逃がすので、そちらを使ってください。
pub struct Hash;

impl Hash {
    /// 保存する形に変える。塩は毎回変わるので、**同じパスワードでも結果は毎回違います。**
    pub fn make(password: &str) -> String {
        Self::make_with(password, iterations())
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
    pub fn check(password: &str, hashed: &str) -> bool {
        let Some(parsed) = Parsed::parse(hashed) else {
            tracing::warn!("保存されているパスワードの形が正しくありません");
            return false;
        };
        let digest = crypto::pbkdf2_sha256(password.as_bytes(), &parsed.salt, parsed.iterations);
        crypto::constant_time_eq(&digest, &parsed.digest)
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
    pub(crate) fn waste_time() {
        static DUMMY: OnceLock<String> = OnceLock::new();
        let dummy = DUMMY.get_or_init(|| Hash::make("bengara-dummy-password"));
        let _ = Hash::check("bengara-dummy-password", dummy);
    }
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
}
