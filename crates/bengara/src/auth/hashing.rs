//! パスワードの変換（ハッシュ）。
//!
//! 方式は **PBKDF2-HMAC-SHA256**（RFC 8018）です。土台は `support::crypto` の
//! HMAC-SHA256 なので、依存クレートは増えません（決定記録 #046）。
//!
//! ```ignore
//! let stored = Hash::make("ひみつ");
//! assert!(Hash::check("ひみつ", &stored));
//! ```

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::error::Result;
use crate::support::crypto;

/// 既定の繰り返し回数。
///
/// リリースビルドで 1 回の照合が約 0.2 秒になる回数です。
///
/// 120,000 回だったころと同じ時間です。PBKDF2 の 1 回の繰り返しを圧縮 4 回から
/// 2 回に減らして速さがほぼ倍になったので、回数を倍にして強さを上げました。
/// 速すぎると総当たりに弱く、遅すぎるとログインが重くなります。
pub(crate) const DEFAULT_ITERATIONS: u32 = 240_000;

/// テストのときに使う回数。
///
/// デバッグビルドのテストで既定の 240,000 回を回すと 1 件あたり 4 秒ほどかかるので、
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
/// いくらでも引き延ばせます（既定の 240,000 回で 0.2 秒ほどなので、
/// 1,000 万回なら 8 秒ほど）。上限を超える値は照合せずに偽とします。
const MAX_ITERATIONS: u32 = 10_000_000;

/// 照合に渡せるパスワードの長さの上限（バイト）。
///
/// PBKDF2 の計算量は長さに左右されませんが、入口の `sha256` だけは長さに比例します。
/// 2 MB の文字列を毎回ハッシュする意味は無いので、ここで断ります。
/// 人が覚えて入力するパスワードは、これより桁違いに短いです。
///
/// **4096 なのは、検査の `max:1024`（文字数で数える）が必ず収まるようにするためです。**
/// UTF-8 は 1 文字が最大 4 バイトなので、1024 文字は最大 4096 バイトです。
/// 1024 バイトにしていたころは、`max:1024` を付けていても、日本語の長いパスワードで
/// 登録した人が二度とログインできませんでした。
const MAX_PASSWORD_BYTES: usize = 4096;

/// ハッシュの設定。`config/hashing.rs` が返します。
///
/// ```ignore
/// pub fn config() -> HashConfig {
///     HashConfig { iterations: env("HASH_ITERATIONS", 240_000u32) }
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
///
/// **空の値（`HASH_ITERATIONS=`）は指定していない扱いです**（`APP_KEY` と同じ）。
/// 空を「指定あり」と見ると、テストが既定の 240,000 回で走り、1 件 4 秒ほどかかります。
pub(crate) fn install_test_iterations() {
    if hash_iterations_given(std::env::var("HASH_ITERATIONS").ok().as_deref()) {
        return;
    }
    let _ = TEST_OVERRIDE.set(TEST_ITERATIONS);
}

/// 環境変数 `HASH_ITERATIONS` の値が「指定あり」か。前後の空白を落として空なら偽。
fn hash_iterations_given(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

/// いま使う回数。
///
/// 上限（[`MAX_ITERATIONS`]）で抑えます。上限を超える設定のまま作ると、
/// `check` が上限で断るので、作った値で二度とログインできません。
pub(crate) fn iterations() -> u32 {
    if let Some(found) = TEST_OVERRIDE.get() {
        return *found;
    }
    static FALLBACK: OnceLock<HashConfig> = OnceLock::new();
    let configured = crate::try_config::<HashConfig>()
        .unwrap_or_else(|| FALLBACK.get_or_init(HashConfig::default))
        .iterations;
    // 設定の値は照合のたびにここを通るので、警告はプロセスで 1 回だけにします。
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let clamped = clamp_iterations(configured);
    if clamped != configured.max(1) && !WARNED.swap(true, Ordering::Relaxed) {
        warn_clamped("HASH_ITERATIONS（config/hashing.rs）", configured);
    }
    clamped
}

/// 回数を 1 以上・上限（[`MAX_ITERATIONS`]）以下にそろえる。
fn clamp_iterations(iterations: u32) -> u32 {
    iterations.clamp(1, MAX_ITERATIONS)
}

/// 回数を上限で抑えたことを知らせる。
fn warn_clamped(source: &str, iterations: u32) {
    tracing::warn!(
        "{source} の繰り返し回数が多すぎます（{iterations} 回）。\
         上限の {MAX_ITERATIONS} 回で計算します"
    );
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
    ///
    /// # 登録の検査に `max:1024` を必ず付けてください
    ///
    /// `make` は長さを断れません（`String` を返すので、エラーを返す手段がありません）。
    /// 一方 [`check`](Hash::check) は **4096 バイトを超える平文を計算せずに偽とします。**
    /// つまり **4096 バイトを超える値で作ったハッシュは、`check` が必ず偽を返します。**
    /// その利用者は登録はできるのに、二度とログインできません。
    ///
    /// 付け忘れを防ぐために、パスワードを受け取る検査には**例外なく**
    /// `max:1024` を書いてください（`"required|min:8|max:1024|confirmed"` の形）。
    /// `max` は文字数で数えます。UTF-8 は 1 文字が最大 4 バイトなので、
    /// 1024 文字までなら、どの文字でも 4096 バイトに収まります。
    /// 上限を超える平文を渡したときは `error` でログに出しますが、
    /// 処理は止まりません。ログを見ていなければ気付けません。
    pub fn make(password: &str) -> String {
        Self::make_with(password, iterations())
    }

    /// `make` を裏のスレッドで行う。**非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// let password = Hash::make_async(&input).await?;
    /// ```
    ///
    /// **`Err` になるのは裏のスレッドが落ちたときだけです。** 長さは断りません。
    /// [`make`](Hash::make) と同じで、**登録の検査に `max:1024` が必ず要ります。**
    /// 超えた値で作ったハッシュは [`check`](Hash::check) が必ず偽を返します。
    pub async fn make_async(password: &str) -> Result<String> {
        let password = password.to_string();
        crate::support::blocking(move || Self::make(&password))
            .await
            .map_err(|e| crate::Error::msg(format!("パスワードの計算に失敗しました: {e}")))
    }

    /// 回数を指定して変える。
    ///
    /// [`make`](Hash::make) と同じで、長さは断りません。
    /// **4096 バイトを超える値で作ったハッシュは [`check`](Hash::check) が必ず偽を返します。**
    /// 登録の検査に `max:1024` を必ず付けてください。
    ///
    /// 回数は 10,000,000 回で抑えます（警告を出します）。それより多い値は
    /// `check` が照合せずに偽とするためです。
    pub fn make_with(password: &str, iterations: u32) -> String {
        // 上限を超える平文は、作れても二度と照合できない（`check` が断る）。
        // 戻り値の型を変えられないので、せめて作ったところで知らせる。
        // 「その人は二度とログインできない」ので `warn` ではなく `error`。
        if password.len() > MAX_PASSWORD_BYTES {
            tracing::error!(
                "パスワードが長すぎます（{} バイト、上限は {MAX_PASSWORD_BYTES} バイト）。\
                 この値は `check` が常に偽を返すので、ログインできません。\
                 登録の検査に `max:1024`（文字数）を足してください",
                password.len()
            );
        }
        if iterations > MAX_ITERATIONS {
            warn_clamped("Hash::make_with", iterations);
        }
        let iterations = clamp_iterations(iterations);
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
    /// 平文が 4096 バイトを超えるときも、計算せずに偽を返します。
    ///
    /// **非同期の処理の中では [`check_async`](Hash::check_async) を使ってください。**
    /// ここでは 0.2 秒ほどスレッドを塞ぎます。
    pub fn check(password: &str, hashed: &str) -> bool {
        check_inner(password, hashed, true)
    }
}

/// [`Hash::check`] の本体。
///
/// `remember` が真なら、読めた回数を [`MAX_SEEN_ITERATIONS`] に覚えます。
/// 空回し（[`Hash::waste_time`]）は偽を渡します。空回しの回数で書き換えると、
/// 覚えている値が本物の照合と関係なく動くためです。
///
/// # 読めない値・回数が多すぎる値でも、同じだけ計算する
///
/// 計算せずにすぐ偽を返すと、「この利用者の保存値は壊れている」ことが応答の速さで
/// 分かります。外れると決まっていても、空回しと同じ回数の PBKDF2 を 1 回通します。
///
/// # 保存されている回数が少ない値も、同じだけ計算する
///
/// 照合は保存されている回数で計算します。`HASH_ITERATIONS` を上げたあと、
/// 作り直していない値の照合はそのぶん速く終わります。空回しのほうは
/// 多いほうに合わせるので、速さの差で「登録されている」ことが分かります。
/// そこで、答えを出したあと、空回しと同じ回数（[`dummy_iterations`]）に
/// 届くまで HMAC を空回しします。
fn check_inner(password: &str, hashed: &str, remember: bool) -> bool {
    // 長すぎる入力は計算せずに断る。合うはずのない値に時間を使わない。
    // 入力の長さは送った本人が知っている値なので、速さで漏れるものはありません。
    if password.len() > MAX_PASSWORD_BYTES {
        tracing::warn!(
            "パスワードが長すぎます（{} バイト、上限は {MAX_PASSWORD_BYTES} バイト）",
            password.len()
        );
        return false;
    }
    let Some(parsed) = Parsed::parse(hashed) else {
        tracing::warn!("保存されているパスワードの形が正しくありません");
        burn(password);
        return false;
    };
    // 回数は保存されている値から取るので、上限を決めておく。
    if parsed.iterations > MAX_ITERATIONS {
        tracing::warn!(
            "保存されているパスワードの繰り返し回数が多すぎます（{} 回、上限は {MAX_ITERATIONS} 回）",
            parsed.iterations
        );
        burn(password);
        return false;
    }
    // 空回しが同じだけ計算できるよう、保存されていた回数の最大を覚えておく。
    if remember {
        MAX_SEEN_ITERATIONS.fetch_max(parsed.iterations, Ordering::Relaxed);
    }
    // 少ない回数で保存された値も、空回しと同じ回数ぶんの時間をかける。
    let spend = parsed.iterations.max(dummy_iterations());
    let digest =
        crypto::pbkdf2_sha256_spending(password.as_bytes(), &parsed.salt, parsed.iterations, spend);
    crypto::constant_time_eq(&digest, &parsed.digest)
}

/// 外れると決まった照合で、空回しと同じだけ PBKDF2 を回す。
fn burn(password: &str) {
    let salt = [0u8; SALT_BYTES];
    std::hint::black_box(crypto::pbkdf2_sha256(
        password.as_bytes(),
        &salt,
        dummy_iterations(),
    ));
}

impl Hash {
    /// `check` を裏のスレッドで行う。**非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// if Hash::check_async(&input, user.password_hash()).await? { ... }
    /// ```
    pub async fn check_async(password: &str, hashed: &str) -> Result<bool> {
        let password = password.to_string();
        let hashed = hashed.to_string();
        crate::support::blocking(move || Self::check(&password, &hashed))
            .await
            .map_err(|e| crate::Error::msg(format!("パスワードの計算に失敗しました: {e}")))
    }

    /// 保存してある値を作り直したほうがよいか。
    ///
    /// 回数の設定を上げたあと、ログインのついでに作り直すのに使います。
    /// `req.auth().attempt(...)` は、合ったときにこれを見て自動で作り直します。
    ///
    /// 真になるのは次のときです。
    ///
    /// - 保存されている回数が、いまの設定より少ない。
    /// - 保存されている回数が上限（10,000,000 回）を超えている。この値は
    ///   `check` が照合せずに偽とするので、作り直さないとログインできません。
    /// - 値の形が読めない。
    ///
    /// **回数を上げたら、作り直してください。** 作り直さないかぎり、
    /// 保存されている値は古い回数のままです。
    pub fn needs_rehash(hashed: &str) -> bool {
        match Parsed::parse(hashed) {
            Some(parsed) => parsed.iterations < iterations() || parsed.iterations > MAX_ITERATIONS,
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
    ///
    /// **送られてきたパスワードをそのまま渡します。** 固定のダミーを使うと、
    /// 入力の長さで差が出る処理（入口の `sha256`、長さの上限の判定）が
    /// 登録済みの側にだけ効き、登録の有無が時間差で分かってしまいます。
    /// 比べる相手だけが固定のダミーです。
    ///
    /// **回数は「いまの設定」と「これまでに照合した値に書いてあった回数の最大」の
    /// 大きいほう**を使います（[`dummy_iterations`]）。本物の照合も、保存されている
    /// 回数がそれより少なければ、同じ回数ぶんの時間をかけます。
    ///
    /// - いまの設定だけで計算していたころは、`HASH_ITERATIONS` を上げて再ハッシュして
    ///   いない間だけ空回しのほうが重くなり、1 リクエストで利用者の有無が分かりました。
    /// - 最後に見た回数で計算していたころは、照合の順番しだいで空回しが軽くも重くも
    ///   なりました。本物の照合も、古い回数の値だけ速く終わっていました。
    ///
    /// 空回しと本物の照合を同じ回数にそろえるので、どちらが速いかで見分けられません。
    /// 回数が多い値を一度でも照合すると、プロセスを起動し直すまで
    /// すべての照合がその回数ぶんかかります（上限は [`MAX_ITERATIONS`]）。
    pub(crate) fn waste_time(password: &str) {
        let _ = check_inner(password, &dummy_hash(dummy_iterations()), false);
    }
}

/// これまでに照合した値に書いてあった繰り返し回数の最大。0 は「まだ読んでいない」。
///
/// 見つかった側の `check` は**保存されている**回数で計算します。
/// 空回しをそれに合わせるため、読めた回数の最大をここに覚えます。
/// 覚えるのは上限（[`MAX_ITERATIONS`]）を通った値だけです。
///
/// 「最後に読んだ回数」にしていたころは、照合の順番で空回しの重さが変わりました。
/// 最大なら、一度上がったら下がりません。
static MAX_SEEN_ITERATIONS: AtomicU32 = AtomicU32::new(0);

/// 照合 1 回にかける回数。いまの設定と、これまでに読んだ回数の最大の大きいほうです。
///
/// 空回しはこの回数で計算します。本物の照合も、保存されている回数が
/// これより少なければ、この回数ぶんの時間をかけます。
fn dummy_iterations() -> u32 {
    dummy_iterations_from(MAX_SEEN_ITERATIONS.load(Ordering::Relaxed))
}

/// 覚えている回数から、空回しに使う回数を決める。
///
/// 覚えている値を引数にして、テストから確かめられる形にしています
/// （静的な値はテストが並んで走ると書き換わります）。
///
/// 上限（[`MAX_ITERATIONS`]）を超えることはありません。覚えるのは上限を通った値だけで、
/// 設定のほうも念のためここで抑えます。
fn dummy_iterations_from(max_seen: u32) -> u32 {
    iterations().max(max_seen).min(MAX_ITERATIONS)
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
    /// `$pbkdf2-sha256$i=240000$<塩>$<ハッシュ>` を分解する。
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
        install_test_iterations();
        let stored = Hash::make_with("ひみつの言葉", FAST);
        assert!(Hash::check("ひみつの言葉", &stored));
        assert!(!Hash::check("ちがう言葉", &stored));
        assert!(!Hash::check("", &stored));
    }

    #[test]
    fn 同じパスワードでも毎回違う値になる() {
        install_test_iterations();
        let a = Hash::make_with("same", FAST);
        let b = Hash::make_with("same", FAST);
        assert_ne!(a, b, "塩が毎回変わる");
        assert!(Hash::check("same", &a));
        assert!(Hash::check("same", &b));
    }

    #[test]
    fn 保存する形に方式と回数が入っている() {
        install_test_iterations();
        let stored = Hash::make_with("x", 123);
        assert!(stored.starts_with("$pbkdf2-sha256$i=123$"), "{stored}");
        // 方式 + 回数 + 塩(32) + ハッシュ(64) + 区切り
        assert_eq!(stored.matches('$').count(), 4);
    }

    #[test]
    fn 回数が違っても照合できる() {
        install_test_iterations();
        let stored = Hash::make_with("x", 7);
        assert!(Hash::check("x", &stored), "保存した値の回数を使う");
    }

    #[test]
    fn 壊れた値は偽になる() {
        // 壊れた値でも空回しと同じだけ計算するので、回数を下げておく。
        install_test_iterations();
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
        install_test_iterations();
        let few = Hash::make_with("x", 1);
        assert!(Hash::needs_rehash(&few), "設定より少ない");

        // 多い側は**計算しない**（上限近くを実際に回すと終わらない）。
        // 保存されている形だけを組み立てて判定させる。
        let many = format!("$pbkdf2-sha256$i={MAX_ITERATIONS}$00${}", "ab".repeat(32));
        assert!(!Hash::needs_rehash(&many), "設定より多いが上限以内");

        // 上限を超える値は `check` が必ず偽にするので、作り直す対象。
        // 以前は「設定より多い」として偽を返し、二度とログインできないままだった。
        for over in [MAX_ITERATIONS + 1, u32::MAX] {
            let stored = format!("$pbkdf2-sha256$i={over}$00${}", "ab".repeat(32));
            assert!(Hash::needs_rehash(&stored), "{over} 回");
        }
    }

    #[test]
    fn 回数は上限で抑える() {
        // 上限を超える回数で作ると、`check` が断るので二度と照合できなかった。
        assert_eq!(clamp_iterations(0), 1);
        assert_eq!(clamp_iterations(37), 37);
        assert_eq!(clamp_iterations(MAX_ITERATIONS), MAX_ITERATIONS);
        assert_eq!(clamp_iterations(MAX_ITERATIONS + 1), MAX_ITERATIONS);
        assert_eq!(clamp_iterations(u32::MAX), MAX_ITERATIONS);
    }

    #[test]
    fn 回数が少ない値の照合も空回しと同じ回数ぶん計算する() {
        // 保存されている回数のぶんしか計算していなかったころは、`HASH_ITERATIONS` を
        // 上げたあと、作り直していない利用者の照合だけ速く終わった。
        // 空回しは多いほうに合わせるので、速さで登録の有無が分かった。
        install_test_iterations();
        let target = dummy_iterations();
        assert!(target > 10, "テストの回数は 10 より多い");

        // 回数 1 の値と、空回しの回数の値で、計算する回数（spend）が同じになること。
        // 時間は揺れるので、回数で確かめる。
        let few = Parsed::parse(&Hash::make_with("ひみつ", 1)).unwrap();
        assert_eq!(few.iterations.max(dummy_iterations()), dummy_iterations());

        // 答えは変わらない（空回しは答えに混ぜない）。
        let stored = Hash::make_with("ひみつ", 1);
        assert!(Hash::check("ひみつ", &stored));
        assert!(!Hash::check("ちがう", &stored));

        // 時間でも大まかに確かめる。回数 1 の値の照合が、空回しより極端に
        // 速くはならない（以前は 1 回ぶんで即座に終わっていた）。
        let started = std::time::Instant::now();
        Hash::waste_time("ひみつ");
        let dummy = started.elapsed();
        let started = std::time::Instant::now();
        assert!(Hash::check("ひみつ", &stored));
        let real = started.elapsed();
        assert!(real * 10 >= dummy, "本物 {real:?} / 空回し {dummy:?}");
    }

    #[test]
    fn 覚える回数は最大で下がらない() {
        // 最後に見た回数を覚えていたころは、古い回数の値を照合した直後に
        // 空回しが軽くなった。最大を覚えるので、少ない値を照合しても下がらない。
        install_test_iterations();
        let more = Hash::make_with("ひみつ", 41);
        assert!(Hash::check("ひみつ", &more));
        let seen = MAX_SEEN_ITERATIONS.load(Ordering::Relaxed);
        assert!(seen >= 41, "{seen}");

        let fewer = Hash::make_with("ひみつ", 3);
        assert!(Hash::check("ひみつ", &fewer));
        assert!(
            MAX_SEEN_ITERATIONS.load(Ordering::Relaxed) >= seen,
            "少ない値を照合しても下がらない"
        );
    }

    #[test]
    fn 既定の回数は実測に合わせた値() {
        assert_eq!(DEFAULT_ITERATIONS, 240_000);
        assert_eq!(HashConfig::default().iterations, DEFAULT_ITERATIONS);
    }

    #[test]
    fn 回数が上限を超えていれば保存された回数では計算せずに偽になる() {
        // 保存された回数で PBKDF2 を回さないこと（回すと終わらない）を、時間でも確かめる。
        // 空回しの回数（テストでは下げてある）だけは回す。
        install_test_iterations();
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

        // 回数は渡した値がそのまま入る。渡すのは「これまでに照合した値に書いてあった
        // 回数の最大」（まだ何も読んでいなければ、いまの設定）なので、本物の照合と
        // 同じだけかかる。
        assert_eq!(Parsed::parse(&dummy_hash(123)).unwrap().iterations, 123);
        assert_eq!(Parsed::parse(&dummy).unwrap().salt.len(), SALT_BYTES);

        // 2 回続けて呼んでも落ちない（回数は下げておく）。
        install_test_iterations();
        Hash::waste_time("ひみつの言葉");
        Hash::waste_time("ひみつの言葉");
    }

    #[test]
    fn 空回しは設定と保存されている回数の大きいほうに合わせる() {
        // 設定だけで計算していたころは、`HASH_ITERATIONS` を上げて再ハッシュして
        // いない間だけ空回しのほうが重くなった。最後に見た回数だけで計算していた
        // ころは、古い回数の利用者を照合した直後だけ空回しが軽くなった。
        install_test_iterations();

        // 設定より多い回数を覚えていれば、そちら。
        assert_eq!(dummy_iterations_from(iterations() + 37), iterations() + 37);
        // 設定より少ない回数を覚えていても、設定まで上げる。
        assert_eq!(dummy_iterations_from(37), iterations().max(37));
        // まだ何も読んでいなければ、いまの設定にそろえる。
        assert_eq!(dummy_iterations_from(0), iterations());
        // 上限は超えない。
        assert_eq!(dummy_iterations_from(u32::MAX), MAX_ITERATIONS);

        // 照合すると、保存されていた回数を覚える。
        // 値そのものは、ほかのテストが並んで走ると書き換わるので見ません。
        let stored = Hash::make_with("ひみつ", 37);
        assert!(Hash::check("ひみつ", &stored));
        assert_ne!(
            MAX_SEEN_ITERATIONS.load(Ordering::Relaxed),
            0,
            "読めた回数を覚える"
        );

        // 空回しも、その回数で計算できる形になっている。
        assert!(Parsed::parse(&dummy_hash(dummy_iterations())).is_some());
        Hash::waste_time("ひみつ");
    }

    #[test]
    fn 空の環境変数は回数の指定とみなさない() {
        assert!(!hash_iterations_given(None));
        assert!(!hash_iterations_given(Some("")));
        assert!(!hash_iterations_given(Some("  ")));
        assert!(hash_iterations_given(Some("50000")));
    }

    #[test]
    fn 上限は1024文字ならどの文字でも収まる() {
        install_test_iterations();
        // 検査の `max:1024` は文字数で数える。UTF-8 は 1 文字が最大 4 バイト。
        let emoji = "😀".repeat(1024);
        assert_eq!(emoji.chars().count(), 1024);
        assert_eq!(emoji.len(), MAX_PASSWORD_BYTES);
        let stored = Hash::make_with(&emoji, FAST);
        assert!(Hash::check(&emoji, &stored), "登録した人がログインできる");

        let kana = "あ".repeat(1024);
        let stored = Hash::make_with(&kana, FAST);
        assert!(Hash::check(&kana, &stored));
    }

    #[test]
    fn 読めない値でも空回しと同じだけ計算する() {
        // すぐ偽を返していたころは、「保存値が壊れている利用者」が速さで分かった。
        // ここでは時間は測らず、経路が落ちずに偽を返すことと、覚えている回数を
        // 書き換えないことを確かめる。
        install_test_iterations();
        let huge = format!(
            "$pbkdf2-sha256$i={}$00${}",
            MAX_ITERATIONS + 1,
            "ab".repeat(32)
        );
        assert!(!Hash::check("x", &huge));
        assert!(!Hash::check("x", "壊れた値"));
        assert_ne!(
            MAX_SEEN_ITERATIONS.load(Ordering::Relaxed),
            MAX_ITERATIONS + 1,
            "上限を超えた回数は覚えない"
        );
    }

    #[test]
    fn 空回しは送られてきたパスワードを使う() {
        // 固定のダミーを使っていたころは、長い入力のときだけ
        // 「登録済み」側が重くなり、登録の有無が時間差で漏れていた。
        // いまは本物と同じ値を同じ経路に通すので、差が出ない。
        install_test_iterations();
        let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
        // 上限を超える入力も、本物の `check` と同じようにすぐ帰る。
        Hash::waste_time(&long);
        Hash::waste_time("");
    }

    #[test]
    fn 長すぎるパスワードは計算せずに偽になる() {
        install_test_iterations();
        let stored = Hash::make_with("ok", FAST);
        assert!(Hash::check("ok", &stored));

        // 上限ちょうどは断らない（合わないだけ）。
        let limit = "a".repeat(MAX_PASSWORD_BYTES);
        assert!(!Hash::check(&limit, &stored), "計算はするが合わない");

        // 1 バイト超えたら計算せずに偽。
        let over = "a".repeat(MAX_PASSWORD_BYTES + 1);
        let started = std::time::Instant::now();
        assert!(!Hash::check(&over, &stored));
        assert!(started.elapsed().as_secs() < 1, "すぐ返る");

        // 上限はバイト数で見る（日本語は 1 文字 3 バイト）。
        let kana = "あ".repeat(MAX_PASSWORD_BYTES / 3 + 1);
        assert!(kana.len() > MAX_PASSWORD_BYTES);
        assert!(!Hash::check(&kana, &stored));
    }

    #[test]
    fn 上限を超える平文でも作れるが照合できない() {
        install_test_iterations();
        // `make` は `String` を返すので断れない。落ちずに値を作り、警告だけ出す。
        let over = "a".repeat(MAX_PASSWORD_BYTES + 1);
        let stored = Hash::make_with(&over, FAST);
        assert!(stored.starts_with("$pbkdf2-sha256$"), "{stored}");

        // 作れてしまうが、`check` は上限で断るので二度と合わない。
        assert!(!Hash::check(&over, &stored), "登録できてもログインできない");
    }

    #[test]
    fn 長いパスワードでも照合の時間が変わらない() {
        install_test_iterations();
        // PBKDF2 の鍵のそろえ直しが入口の 1 回だけになったことを、時間で確かめる。
        // 直す前は、繰り返しのたびにパスワード全体を `sha256` していた。
        let long = "x".repeat(MAX_PASSWORD_BYTES);
        let stored = Hash::make_with(&long, 2_000);
        let started = std::time::Instant::now();
        assert!(Hash::check(&long, &stored));
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 3, "{elapsed:?} かかった");
    }

    #[tokio::test]
    async fn 非同期版でも往復できる() {
        // 既定の 240,000 回はデバッグビルドで 1 件 4 秒ほどかかるので下げる。
        install_test_iterations();

        let stored = Hash::make_async("ひみつの言葉").await.unwrap();
        assert!(Hash::check_async("ひみつの言葉", &stored).await.unwrap());
        assert!(!Hash::check_async("ちがう言葉", &stored).await.unwrap());
    }
}
