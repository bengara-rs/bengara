//! パスワードの変換（ハッシュ）。
//!
//! 既定の方式は **PBKDF2-HMAC-SHA256**（RFC 8018）です。土台は `support::crypto` の
//! HMAC-SHA256 なので、依存クレートは増えません（決定記録 #046）。
//!
//! 機能フラグ `argon2` を入れて `HASH_DRIVER=argon2id` にすると、**Argon2id** で作ります。
//! 照合は保存されている値の先頭（`$pbkdf2-sha256$` か `$argon2id$`）で振り分けるので、
//! 方式を替えても古い値で照合できます。ログインのついでに新しい方式で作り直します。
//!
//! ```ignore
//! let stored = Hash::make("ひみつ");
//! assert!(Hash::check("ひみつ", &stored));
//! ```

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::OnceLock;

use tokio::sync::Semaphore;

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

/// PBKDF2 の保存値の先頭。
const PBKDF2_PREFIX: &str = "$pbkdf2-sha256$";

/// Argon2id の保存値の先頭。
const ARGON2ID_PREFIX: &str = "$argon2id$";

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

/// Argon2id の既定のメモリ（KiB）。Laravel 12 の `config/hashing.php` と同じです。
pub(crate) const DEFAULT_ARGON_MEMORY: u32 = 65_536;

/// Argon2id の既定の繰り返し。Laravel と同じです。
pub(crate) const DEFAULT_ARGON_TIME: u32 = 4;

/// Argon2id の既定の並列の数。Laravel と同じです。
pub(crate) const DEFAULT_ARGON_THREADS: u32 = 1;

/// Argon2id の並列の数の上限。
const MAX_ARGON_THREADS: u32 = 16;

/// Argon2id のメモリの上限（KiB）。1 GiB です。
///
/// 保存値の `m=` もこの値で抑えます。PBKDF2 の [`MAX_ITERATIONS`] と同じで、
/// 細工した値で 1 回の照合にメモリと時間をいくらでも使わせないためです。
const MAX_ARGON_MEMORY: u32 = 1_048_576;

/// Argon2id の繰り返しの上限。
const MAX_ARGON_TIME: u32 = 64;

/// テストのときに使う Argon2id の設定。クレートが受け付ける最小に近い値です。
const TEST_ARGON: ArgonConfig = ArgonConfig {
    memory: 8,
    time: 1,
    threads: 1,
};

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

/// Argon2id の設定。使うのは機能フラグ `argon2` と `HASH_DRIVER=argon2id` のときだけです。
///
/// 名前と既定値は Laravel 12 の `config/hashing.php` の `argon` に合わせてあります。
/// 設定として登録されていなければ、環境変数 `ARGON_MEMORY`・`ARGON_TIME`・`ARGON_THREADS`
/// を読みます。
///
/// 範囲の外の値は寄せて使います（警告を 1 回出します）。
///
/// | 項目      | 範囲                       |
/// |-----------|----------------------------|
/// | `threads` | 1〜16                      |
/// | `memory`  | `8 × threads` 〜 1,048,576 |
/// | `time`    | 1〜64                      |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgonConfig {
    /// 使うメモリ（KiB）。既定は 65,536（64 MiB）。
    pub memory: u32,
    /// 繰り返し。既定は 4。
    pub time: u32,
    /// 並列の数。既定は 1。
    pub threads: u32,
}

impl Default for ArgonConfig {
    /// 環境変数 `ARGON_MEMORY`・`ARGON_TIME`・`ARGON_THREADS` から作る。
    fn default() -> Self {
        Self {
            memory: crate::env("ARGON_MEMORY", DEFAULT_ARGON_MEMORY),
            time: crate::env("ARGON_TIME", DEFAULT_ARGON_TIME),
            threads: crate::env("ARGON_THREADS", DEFAULT_ARGON_THREADS),
        }
    }
}

impl ArgonConfig {
    /// 範囲に寄せる。並列の数を先に決め、メモリの下限はそれに合わせます。
    fn clamped(self) -> Self {
        let threads = self.threads.clamp(1, MAX_ARGON_THREADS);
        Self {
            memory: self.memory.clamp(8 * threads, MAX_ARGON_MEMORY),
            time: self.time.clamp(1, MAX_ARGON_TIME),
            threads,
        }
    }
}

/// テストのときに使う回数の上書き。`testing` から入れます。
static TEST_OVERRIDE: OnceLock<u32> = OnceLock::new();

/// テストのときに使う Argon2id の設定の上書き。`testing` から入れます。
static TEST_ARGON_OVERRIDE: OnceLock<ArgonConfig> = OnceLock::new();

/// テスト用に回数を下げる。`#[bengara::test]` が 1 回だけ呼びます。
///
/// **`.env` を読む前**に呼ばれるので、見るのは本物の環境変数だけです。
/// `HASH_ITERATIONS=50000 cargo test` のように指定したときだけ、その値を使います。
/// `.env` に書いた値（開発用に下げた値など）はテストに影響しません。
///
/// Argon2id も同じ考え方で下げます。`ARGON_MEMORY`・`ARGON_TIME`・`ARGON_THREADS` の
/// どれも指定されていなければ、`m=8, t=1, p=1` で計算します。
///
/// **空の値（`HASH_ITERATIONS=`）は指定していない扱いです**（`APP_KEY` と同じ）。
/// 空を「指定あり」と見ると、テストが既定の 240,000 回で走り、1 件 4 秒ほどかかります。
pub(crate) fn install_test_iterations() {
    if !env_given(std::env::var("HASH_ITERATIONS").ok().as_deref()) {
        let _ = TEST_OVERRIDE.set(TEST_ITERATIONS);
    }
    let argon_given = ["ARGON_MEMORY", "ARGON_TIME", "ARGON_THREADS"]
        .iter()
        .any(|key| env_given(std::env::var(key).ok().as_deref()));
    if !argon_given {
        let _ = TEST_ARGON_OVERRIDE.set(TEST_ARGON);
    }
}

/// 環境変数の値が「指定あり」か。前後の空白を落として空なら偽。
fn env_given(value: Option<&str>) -> bool {
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
    static WARNED: AtomicBool = AtomicBool::new(false);
    let clamped = clamp_iterations(configured);
    if clamped != configured.max(1) && !WARNED.swap(true, Ordering::Relaxed) {
        warn_clamped("HASH_ITERATIONS（config/hashing.rs）", configured);
    }
    clamped
}

/// いま使う Argon2id の設定。範囲の外の値は寄せます。
///
/// 読み方は [`iterations`] と同じです。登録された設定（`ArgonConfig`）があればそれを、
/// 無ければ環境変数から作った値を使います。
pub(crate) fn argon_config() -> ArgonConfig {
    if let Some(found) = TEST_ARGON_OVERRIDE.get() {
        return *found;
    }
    static FALLBACK: OnceLock<ArgonConfig> = OnceLock::new();
    let configured = *crate::try_config::<ArgonConfig>()
        .unwrap_or_else(|| FALLBACK.get_or_init(ArgonConfig::default));
    let clamped = configured.clamped();
    static WARNED: AtomicBool = AtomicBool::new(false);
    if clamped != configured && !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "Argon2id の設定（ARGON_MEMORY・ARGON_TIME・ARGON_THREADS）が範囲の外です\
             （m={}, t={}, p={}）。m={}, t={}, p={} で計算します",
            configured.memory,
            configured.time,
            configured.threads,
            clamped.memory,
            clamped.time,
            clamped.threads
        );
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

/// パスワードを作る方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Driver {
    /// PBKDF2-HMAC-SHA256。既定。
    Pbkdf2,
    /// Argon2id。機能フラグ `argon2` が要る。
    Argon2id,
}

/// `HASH_DRIVER` から決めた方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DriverChoice {
    /// 実際に使う方式。
    driver: Driver,
    /// `argon2id` を頼まれたのに、フラグが無くて PBKDF2 に落ちたか。
    missing_flag: bool,
}

/// `HASH_DRIVER` の値を読む。前後の空白を落とし、小文字にして比べる。知らない値は `None`。
fn parse_driver(raw: &str) -> Option<Driver> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pbkdf2" => Some(Driver::Pbkdf2),
        "argon2id" => Some(Driver::Argon2id),
        _ => None,
    }
}

/// `HASH_DRIVER` の値から、実際に使う方式を決める。ログはここで出す。
fn resolve_driver(raw: &str) -> DriverChoice {
    let pbkdf2 = DriverChoice {
        driver: Driver::Pbkdf2,
        missing_flag: false,
    };
    match parse_driver(raw) {
        Some(Driver::Pbkdf2) => pbkdf2,
        Some(Driver::Argon2id) if cfg!(feature = "argon2") => DriverChoice {
            driver: Driver::Argon2id,
            missing_flag: false,
        },
        Some(Driver::Argon2id) => {
            tracing::error!(
                "HASH_DRIVER=argon2id には Cargo.toml の bengara の features に \"argon2\" が要ります。\
                 pbkdf2 で作ります"
            );
            DriverChoice {
                missing_flag: true,
                ..pbkdf2
            }
        }
        None => {
            tracing::warn!(
                "HASH_DRIVER `{}` は知りません。pbkdf2 を使います",
                raw.trim()
            );
            pbkdf2
        }
    }
}

/// いまの方式。プロセスで 1 回だけ決めます（起動後は読むだけ）。
fn driver_choice() -> DriverChoice {
    static CHOICE: OnceLock<DriverChoice> = OnceLock::new();
    *CHOICE.get_or_init(|| resolve_driver(&crate::env::<String>("HASH_DRIVER", "pbkdf2")))
}

/// `about` に出す、いま実際に使う方式と強さ。
pub(crate) fn driver_label() -> String {
    let choice = driver_choice();
    match choice.driver {
        Driver::Argon2id => {
            let argon = argon_config();
            format!(
                "argon2id（m={}, t={}, p={}）",
                argon.memory, argon.time, argon.threads
            )
        }
        Driver::Pbkdf2 if choice.missing_flag => {
            format!("pbkdf2（{} 回。argon2 フラグがありません）", iterations())
        }
        Driver::Pbkdf2 => format!("pbkdf2（{} 回）", iterations()),
    }
}

/// 作る・照合するときに使う方式と設定。
///
/// 方式は `OnceLock` で固定されるので、単体テストはこれを組み立てて内部の関数に渡し、
/// 両方の方式を確かめます。公開の関数は [`Policy::current`] を渡すだけです。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Policy {
    driver: Driver,
    argon: ArgonConfig,
}

impl Policy {
    /// いまの方式と設定。
    fn current() -> Self {
        Self {
            driver: driver_choice().driver,
            argon: argon_config(),
        }
    }
}

/// 「PBKDF2 の値を照合した」印。
const SEEN_PBKDF2: u8 = 1;

/// 「Argon2id の値を照合した」印。
const SEEN_ARGON2ID: u8 = 2;

/// 照合した方式の印。プロセスの中で一度立ったら下ろしません（[`MAX_SEEN_ITERATIONS`] と同じ）。
///
/// 両方の方式の値が混ざっている間は、照合のたびにもう片方の方式も空回しして、
/// どちらの値の利用者でも、いない利用者でも、同じだけ時間をかけます。
static SEEN: AtomicU8 = AtomicU8::new(0);

/// PBKDF2 の時間をかけるか。方式が PBKDF2 か、PBKDF2 の値を照合したことがあれば真。
fn pbkdf2_cost(policy: Policy, seen: u8) -> bool {
    policy.driver == Driver::Pbkdf2 || seen & SEEN_PBKDF2 != 0
}

/// Argon2id の時間をかけるか。フラグがあり、方式が Argon2id か、
/// Argon2id の値を照合したことがあれば真。
fn argon_cost(policy: Policy, seen: u8) -> bool {
    cfg!(feature = "argon2") && (policy.driver == Driver::Argon2id || seen & SEEN_ARGON2ID != 0)
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
///
/// # 方式
///
/// 既定は PBKDF2 です。機能フラグ `argon2` を入れて `HASH_DRIVER=argon2id` にすると、
/// [`make`](Hash::make) が Argon2id で作ります。[`check`](Hash::check) は保存値の先頭で
/// 方式を見分けるので、どちらの値でも照合できます。
///
/// # 同時に計算する数
///
/// `make_async`・`check_async`・`attempt`・`validate_password` は、同時に計算する数を
/// `HASH_MAX_CONCURRENCY`（既定は CPU の数）までに抑えます。
/// 待たない形の `make` / `check` は、この上限を通りません。
pub struct Hash;

impl Hash {
    /// 保存する形に変える。塩は毎回変わるので、**同じパスワードでも結果は毎回違います。**
    ///
    /// 方式は `HASH_DRIVER` で決まります（既定は PBKDF2）。Argon2id の計算に失敗したときは、
    /// ログにエラーを出して PBKDF2 で作ります（`String` を返すので、失敗を返せません）。
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
        make_using(password, Policy::current())
    }

    /// `make` を裏のスレッドで行う。**非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// let password = Hash::make_async(&input).await?;
    /// ```
    ///
    /// 同時に計算する数は `HASH_MAX_CONCURRENCY` までです。超えた分は空くまで待ちます。
    ///
    /// **`Err` になるのは裏のスレッドが落ちたときだけです。** 長さは断りません。
    /// [`make`](Hash::make) と同じで、**登録の検査に `max:1024` が必ず要ります。**
    /// 超えた値で作ったハッシュは [`check`](Hash::check) が必ず偽を返します。
    pub async fn make_async(password: &str) -> Result<String> {
        let password = password.to_string();
        run_hashing(move || Self::make(&password))
            .await
            .map_err(|e| crate::Error::msg(format!("パスワードの計算に失敗しました: {e}")))
    }

    /// 回数を指定して変える。**方式は `HASH_DRIVER` によらず、いつも PBKDF2 です。**
    ///
    /// [`make`](Hash::make) と同じで、長さは断りません。
    /// **4096 バイトを超える値で作ったハッシュは [`check`](Hash::check) が必ず偽を返します。**
    /// 登録の検査に `max:1024` を必ず付けてください。
    ///
    /// 回数は 10,000,000 回で抑えます（警告を出します）。それより多い値は
    /// `check` が照合せずに偽とするためです。
    pub fn make_with(password: &str, iterations: u32) -> String {
        report_too_long_to_make(password);
        make_pbkdf2(password, iterations)
    }

    /// 平文と、保存してある値を照合する。
    ///
    /// 保存値の先頭で方式を見分けます（`$pbkdf2-sha256$` と、フラグ `argon2` があれば `$argon2id$`）。
    /// 値の形が壊れているとき・知らない方式のときは偽を返します（エラーにはしません）。
    /// 平文が 4096 バイトを超えるときも、計算せずに偽を返します。
    ///
    /// **非同期の処理の中では [`check_async`](Hash::check_async) を使ってください。**
    /// ここでは 0.2 秒ほどスレッドを塞ぎます。
    pub fn check(password: &str, hashed: &str) -> bool {
        check_using(password, hashed, Policy::current(), &SEEN)
    }
}

/// 上限を超える平文で作ろうとしたことを知らせる。
fn report_too_long_to_make(password: &str) {
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
}

/// PBKDF2 で作る。長さの知らせは呼ぶ側で出します。
fn make_pbkdf2(password: &str, iterations: u32) -> String {
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

/// [`Hash::make`] の本体。方式と設定を引数で受け取ります。
fn make_using(password: &str, policy: Policy) -> String {
    report_too_long_to_make(password);
    #[cfg(feature = "argon2")]
    if policy.driver == Driver::Argon2id {
        match argon::make(password, policy.argon) {
            Ok(made) => return made,
            Err(e) => {
                tracing::error!("Argon2id でパスワードを作れませんでした（{e}）。PBKDF2 で作ります")
            }
        }
    }
    #[cfg(not(feature = "argon2"))]
    let _ = policy;
    make_pbkdf2(password, iterations())
}

/// 平文が上限を超えていれば `warn` を出して真。
fn too_long_to_check(password: &str) -> bool {
    // 入力の長さは送った本人が知っている値なので、速さで漏れるものはありません。
    if password.len() > MAX_PASSWORD_BYTES {
        tracing::warn!(
            "パスワードが長すぎます（{} バイト、上限は {MAX_PASSWORD_BYTES} バイト）",
            password.len()
        );
        return true;
    }
    false
}

/// [`Hash::check`] の本体。方式と設定、照合した方式の印を引数で受け取ります。
///
/// 保存値の先頭で振り分けます。読めない値・知らない方式・上限を超える値は、
/// 空回し（[`burn_unreadable`]）をしてから偽を返します。
///
/// # 読めない値・回数が多すぎる値でも、同じだけ計算する
///
/// 計算せずにすぐ偽を返すと、「この利用者の保存値は壊れている」ことが応答の速さで
/// 分かります。外れると決まっていても、空回しと同じだけ計算します。
///
/// # もう片方の方式も空回しする
///
/// 両方の方式の値が混ざっている間（印が立っている間）は、照合のあとにもう片方の方式を
/// 1 回空回しします。いない利用者の空回し（[`Hash::waste_time`]）は両方を回すので、
/// どちらの方式の利用者でも、いない利用者と同じだけかかります。
fn check_using(password: &str, hashed: &str, policy: Policy, seen: &AtomicU8) -> bool {
    // 長すぎる入力は計算せずに断る。合うはずのない値に時間を使わない。
    if too_long_to_check(password) {
        return false;
    }
    if hashed.starts_with(PBKDF2_PREFIX) {
        return check_pbkdf2(password, hashed, policy, seen);
    }
    if hashed.starts_with(ARGON2ID_PREFIX) {
        return check_argon2id(password, hashed, policy, seen);
    }
    tracing::warn!("保存されているパスワードの形が正しくありません（知らない方式です）");
    burn_unreadable(password, policy, seen);
    false
}

/// PBKDF2 の値を照合する。
///
/// # 保存されている回数が少ない値も、同じだけ計算する
///
/// 照合は保存されている回数で計算します。`HASH_ITERATIONS` を上げたあと、
/// 作り直していない値の照合はそのぶん速く終わります。空回しのほうは
/// 多いほうに合わせるので、速さの差で「登録されている」ことが分かります。
/// そこで、答えを出したあと、空回しと同じ回数（[`dummy_iterations`]）に
/// 届くまで HMAC を空回しします。
fn check_pbkdf2(password: &str, hashed: &str, policy: Policy, seen: &AtomicU8) -> bool {
    let Some(parsed) = Parsed::parse(hashed) else {
        tracing::warn!("保存されているパスワードの形が正しくありません");
        burn_unreadable(password, policy, seen);
        return false;
    };
    // 回数は保存されている値から取るので、上限を決めておく。
    if parsed.iterations > MAX_ITERATIONS {
        tracing::warn!(
            "保存されているパスワードの繰り返し回数が多すぎます（{} 回、上限は {MAX_ITERATIONS} 回）",
            parsed.iterations
        );
        burn_unreadable(password, policy, seen);
        return false;
    }
    // 空回しが同じだけ計算できるよう、方式の印と、保存されていた回数の最大を覚えておく。
    let marks = seen.fetch_or(SEEN_PBKDF2, Ordering::Relaxed) | SEEN_PBKDF2;
    MAX_SEEN_ITERATIONS.fetch_max(parsed.iterations, Ordering::Relaxed);
    // 少ない回数で保存された値も、空回しと同じ回数ぶんの時間をかける。
    let spend = parsed.iterations.max(dummy_iterations());
    let digest =
        crypto::pbkdf2_sha256_spending(password.as_bytes(), &parsed.salt, parsed.iterations, spend);
    let ok = crypto::constant_time_eq(&digest, &parsed.digest);
    if argon_cost(policy, marks) {
        burn_argon2id(password, policy.argon);
    }
    ok
}

/// Argon2id の値を照合する。
#[cfg(feature = "argon2")]
fn check_argon2id(password: &str, hashed: &str, policy: Policy, seen: &AtomicU8) -> bool {
    let stored = match argon::parse(hashed) {
        Ok(stored) => stored,
        Err(reason) => {
            tracing::warn!("保存されている Argon2id の値を照合できません（{reason}）");
            burn_unreadable(password, policy, seen);
            return false;
        }
    };
    let marks = seen.fetch_or(SEEN_ARGON2ID, Ordering::Relaxed) | SEEN_ARGON2ID;
    let ok = argon::verify(password, &stored);
    if pbkdf2_cost(policy, marks) {
        burn_pbkdf2(password);
    }
    ok
}

/// Argon2id の値を照合する（フラグなし）。照合できないので、空回しして偽を返す。
#[cfg(not(feature = "argon2"))]
fn check_argon2id(password: &str, _hashed: &str, policy: Policy, seen: &AtomicU8) -> bool {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        #[cfg(test)]
        MISSING_FLAG_REPORTS.fetch_add(1, Ordering::Relaxed);
        tracing::error!("`$argon2id$` の値を照合するには features に \"argon2\" が要ります");
    }
    burn_unreadable(password, policy, seen);
    false
}

/// フラグなしで `$argon2id$` を照合したときにエラーを出した回数（テスト用）。
#[cfg(all(test, not(feature = "argon2")))]
static MISSING_FLAG_REPORTS: AtomicU32 = AtomicU32::new(0);

/// 外れると決まった照合・いない利用者の空回し。
///
/// [`pbkdf2_cost`] なら PBKDF2 を、[`argon_cost`] なら Argon2id を、それぞれ 1 回回します。
fn burn_unreadable(password: &str, policy: Policy, seen: &AtomicU8) {
    let marks = seen.load(Ordering::Relaxed);
    if pbkdf2_cost(policy, marks) {
        burn_pbkdf2(password);
    }
    if argon_cost(policy, marks) {
        burn_argon2id(password, policy.argon);
    }
}

/// 空回しと同じ回数（[`dummy_iterations`]）だけ PBKDF2 を回す。
fn burn_pbkdf2(password: &str) {
    #[cfg(test)]
    counts::bump(&counts::PBKDF2_BURNS);
    let salt = [0u8; SALT_BYTES];
    std::hint::black_box(crypto::pbkdf2_sha256(
        password.as_bytes(),
        &salt,
        dummy_iterations(),
    ));
}

/// 設定の m・t・p で Argon2id を 1 回回す。
#[cfg(feature = "argon2")]
fn burn_argon2id(password: &str, config: ArgonConfig) {
    #[cfg(test)]
    counts::bump(&counts::ARGON_BURNS);
    argon::burn(password, config);
}

/// フラグなしでは呼ばれない（[`argon_cost`] が偽）。
#[cfg(not(feature = "argon2"))]
fn burn_argon2id(_password: &str, _config: ArgonConfig) {}

/// [`Hash::needs_rehash`] の本体。方式と設定を引数で受け取ります。
fn needs_rehash_using(hashed: &str, policy: Policy) -> bool {
    match policy.driver {
        Driver::Pbkdf2 => {
            if let Some(parsed) = Parsed::parse(hashed) {
                return parsed.iterations < iterations() || parsed.iterations > MAX_ITERATIONS;
            }
            // 強い方式から弱い方式へは作り直さない。
            #[cfg(feature = "argon2")]
            if argon::parse(hashed).is_ok() {
                return false;
            }
            // 読めない値は作り直す対象とする。
            true
        }
        Driver::Argon2id => {
            #[cfg(feature = "argon2")]
            if let Ok(stored) = argon::parse(hashed) {
                return stored.memory < policy.argon.memory || stored.time < policy.argon.time;
            }
            // PBKDF2 の値・上限を超える値・読めない値は作り直す。
            true
        }
    }
}

/// いない利用者の空回しの本体。
fn waste_time_using(password: &str, policy: Policy, seen: &AtomicU8) {
    // 本物の `check` と同じく、長すぎる入力はすぐ帰る。
    if too_long_to_check(password) {
        return;
    }
    burn_unreadable(password, policy, seen);
}

impl Hash {
    /// `check` を裏のスレッドで行う。**非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// if Hash::check_async(&input, user.password_hash()).await? { ... }
    /// ```
    ///
    /// 同時に計算する数は `HASH_MAX_CONCURRENCY` までです。超えた分は空くまで待ちます。
    pub async fn check_async(password: &str, hashed: &str) -> Result<bool> {
        let password = password.to_string();
        let hashed = hashed.to_string();
        run_hashing(move || Self::check(&password, &hashed))
            .await
            .map_err(|e| crate::Error::msg(format!("パスワードの計算に失敗しました: {e}")))
    }

    /// 保存してある値を作り直したほうがよいか。
    ///
    /// 設定を上げたあと・方式を替えたあと、ログインのついでに作り直すのに使います。
    /// `req.auth().attempt(...)` は、合ったときにこれを見て自動で作り直します。
    ///
    /// 方式が PBKDF2（既定）のとき、真になるのは次のときです。
    ///
    /// - 保存されている回数が、いまの設定より少ない。
    /// - 保存されている回数が上限（10,000,000 回）を超えている。この値は
    ///   `check` が照合せずに偽とするので、作り直さないとログインできません。
    /// - 値の形が読めない。
    ///
    /// Argon2id の値（フラグ `argon2` があるとき）は偽です。強い方式から弱い方式へは作り直しません。
    ///
    /// 方式が Argon2id（`HASH_DRIVER=argon2id`）のとき、真になるのは次のときです。
    ///
    /// - PBKDF2 の値。
    /// - 保存されている `m` か `t` が、いまの設定より小さい。
    /// - `m`・`t`・`p` が上限を超えている。値の形が読めない。
    ///
    /// **設定を上げたら、作り直してください。** 作り直さないかぎり、
    /// 保存されている値は古い設定のままです。
    pub fn needs_rehash(hashed: &str) -> bool {
        needs_rehash_using(hashed, Policy::current())
    }

    /// 照合にかかる時間を合わせるための空回し。
    ///
    /// 利用者が見つからなかったときに呼びます。何もしないで帰ると、
    /// 「そのメールアドレスは登録されていない」ことが応答の速さから分かってしまいます。
    ///
    /// 回すのは、いまの方式と、これまでに照合した値の方式です（両方のこともあります）。
    /// PBKDF2 は [`dummy_iterations`] 回、Argon2id は設定の m・t・p で 1 回です。
    /// 比べる相手は作りません。計算するだけなので、初回だけ重くなることもありません。
    ///
    /// **送られてきたパスワードをそのまま渡します。** 固定のダミーを使うと、
    /// 入力の長さで差が出る処理（入口の `sha256`、長さの上限の判定）が
    /// 登録済みの側にだけ効き、登録の有無が時間差で分かってしまいます。
    ///
    /// **PBKDF2 の回数は「いまの設定」と「これまでに照合した値に書いてあった回数の最大」の
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
        waste_time_using(password, Policy::current(), &SEEN);
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

/// Argon2id の計算。中身は `argon2` クレートに任せます（判断 30 と同じ考え方）。
#[cfg(feature = "argon2")]
mod argon {
    use super::{ArgonConfig, MAX_ARGON_MEMORY, MAX_ARGON_THREADS, MAX_ARGON_TIME};
    use argon2::password_hash::phc::PasswordHash;
    use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};

    /// 塩の長さ（バイト）。クレートの既定と同じです。
    const SALT_BYTES: usize = 16;

    /// 出力の長さ（バイト）。クレートの既定と同じです。
    const OUTPUT_BYTES: usize = 32;

    /// 読めた保存値。
    pub(super) struct Stored {
        hash: PasswordHash,
        pub(super) memory: u32,
        pub(super) time: u32,
    }

    /// Argon2id・版 19 の計算機。
    fn context(params: Params) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
    }

    /// 設定から計算の引数を作る。設定は寄せてあるので、通常は失敗しません。
    fn params(config: ArgonConfig) -> Result<Params, argon2::Error> {
        Params::new(config.memory, config.time, config.threads, None)
    }

    /// `$argon2id$v=19$m=..,t=..,p=..$<塩>$<ハッシュ>`（PHC 形式）を作る。
    pub(super) fn make(password: &str, config: ArgonConfig) -> Result<String, String> {
        let params = params(config).map_err(|e| e.to_string())?;
        let hash: PasswordHash = context(params)
            .hash_password(password.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(hash.to_string())
    }

    /// 保存値を読む。照合してよい値だけを返します。
    ///
    /// アルゴリズムが `argon2id`、版が 19、m・t・p が上限以内で、塩とハッシュがあるもの。
    pub(super) fn parse(hashed: &str) -> Result<Stored, &'static str> {
        let hash = PasswordHash::new(hashed).map_err(|_| "形が読めません")?;
        if hash.algorithm != argon2::ARGON2ID_IDENT {
            return Err("方式が argon2id ではありません");
        }
        if hash.version != Some(Version::V0x13.into()) {
            return Err("版が 19 ではありません");
        }
        if hash.salt.is_none() || hash.hash.is_none() {
            return Err("塩かハッシュがありません");
        }
        let params = Params::try_from(&hash).map_err(|_| "m・t・p が読めません")?;
        if params.m_cost() > MAX_ARGON_MEMORY
            || params.t_cost() > MAX_ARGON_TIME
            || params.p_cost() > MAX_ARGON_THREADS
        {
            return Err("m・t・p が上限を超えています");
        }
        Ok(Stored {
            memory: params.m_cost(),
            time: params.t_cost(),
            hash,
        })
    }

    /// 保存値の m・t・p で計算して比べる。
    pub(super) fn verify(password: &str, stored: &Stored) -> bool {
        #[cfg(test)]
        super::counts::bump(&super::counts::ARGON_VERIFIES);
        context(Params::DEFAULT)
            .verify_password(password.as_bytes(), &stored.hash)
            .is_ok()
    }

    /// 設定の m・t・p で 1 回計算して捨てる。
    pub(super) fn burn(password: &str, config: ArgonConfig) {
        let Ok(params) = params(config) else {
            return;
        };
        let mut out = [0u8; OUTPUT_BYTES];
        let _ = std::hint::black_box(context(params).hash_password_into(
            password.as_bytes(),
            &[0u8; SALT_BYTES],
            &mut out,
        ));
        std::hint::black_box(out);
    }
}

/// パスワードの計算を同時に走らせる数の上限。
///
/// Argon2id は 1 件で 64 MiB を使います。ログインが一度に押し寄せると、
/// 計算の数だけメモリを取り、裏のスレッドも埋まります。PBKDF2 にも効かせます。
fn hashing_semaphore() -> &'static Semaphore {
    static SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();
    SEMAPHORE.get_or_init(|| Semaphore::new(max_concurrency()))
}

/// `HASH_MAX_CONCURRENCY`。既定は CPU の数（取れなければ 4）。1 未満は 1。
fn max_concurrency() -> usize {
    let default = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    crate::env::<usize>("HASH_MAX_CONCURRENCY", default).clamp(1, Semaphore::MAX_PERMITS)
}

/// パスワードの計算を裏のスレッドで行う。同時に走るのは `HASH_MAX_CONCURRENCY` 件まで。
///
/// permit は計算の間だけ持ちます。パスワードの計算を `spawn_blocking` に出す所は、
/// すべてここを通します。
pub(crate) async fn run_hashing<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_hashing_with(hashing_semaphore(), f).await
}

/// [`run_hashing`] の本体。上限（`Semaphore`）を引数で受け取ります。
async fn run_hashing_with<T, F>(semaphore: &Semaphore, f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let _permit = semaphore
        .acquire()
        .await
        .map_err(|e| crate::Error::msg(format!("パスワードの計算の順番を取れませんでした: {e}")))?;
    crate::support::blocking(f).await
}

/// 計算の回数を数える口（テスト用）。照合は呼んだスレッドで走るので、スレッドごとに数えます。
#[cfg(test)]
mod counts {
    use std::cell::Cell;
    use std::thread::LocalKey;

    thread_local! {
        pub(super) static PBKDF2_BURNS: Cell<u32> = const { Cell::new(0) };
        pub(super) static ARGON_BURNS: Cell<u32> = const { Cell::new(0) };
        pub(super) static ARGON_VERIFIES: Cell<u32> = const { Cell::new(0) };
    }

    pub(super) fn bump(key: &'static LocalKey<Cell<u32>>) {
        key.with(|count| count.set(count.get() + 1));
    }

    /// 数えた値を 0 に戻し、(PBKDF2 の空回し, Argon2id の空回し, Argon2id の照合) を返す。
    pub(super) fn take() -> (u32, u32, u32) {
        (
            PBKDF2_BURNS.with(|c| c.replace(0)),
            ARGON_BURNS.with(|c| c.replace(0)),
            ARGON_VERIFIES.with(|c| c.replace(0)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストでは回数を下げる（1 件ごとに 3 秒かかるのを避ける）。
    const FAST: u32 = 50;

    /// 方式が PBKDF2 のときの設定。
    const PBKDF2: Policy = Policy {
        driver: Driver::Pbkdf2,
        argon: TEST_ARGON,
    };

    /// 方式が Argon2id のときの設定。
    const ARGON: Policy = Policy {
        driver: Driver::Argon2id,
        argon: TEST_ARGON,
    };

    /// PHP の `password_hash`（libargon2）で作った値。パスワードは `ひみつのパスワード`、m=8・t=1・p=1。
    const PHP_ARGON: &str =
        "$argon2id$v=19$m=8,t=1,p=1$d1mA1cLDX6j9Sr4mo0qh7Q$jgxsUIxWxpRM9AR94krprunkuoSS9uNJ1PEivtjxac8";

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
        // いまは相手を作らずに計算だけするので、1 回目と 2 回目で差が出ない。
        install_test_iterations();
        let marks = AtomicU8::new(0);
        counts::take();
        waste_time_using("ひみつの言葉", PBKDF2, &marks);
        assert_eq!(counts::take(), (1, 0, 0), "1 回目");
        waste_time_using("ひみつの言葉", PBKDF2, &marks);
        assert_eq!(counts::take(), (1, 0, 0), "2 回目も同じ");
        // 空回しでは印を立てない。
        assert_eq!(marks.load(Ordering::Relaxed), 0);

        // 公開の口からも、2 回続けて呼んで落ちない。
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
        Hash::waste_time("ひみつ");
    }

    #[test]
    fn 空の環境変数は回数の指定とみなさない() {
        assert!(!env_given(None));
        assert!(!env_given(Some("")));
        assert!(!env_given(Some("  ")));
        assert!(env_given(Some("50000")));
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
        // 経路が落ちずに偽を返すこと、空回しをすること、覚えている回数を
        // 書き換えないことを確かめる。
        install_test_iterations();
        let huge = format!(
            "$pbkdf2-sha256$i={}$00${}",
            MAX_ITERATIONS + 1,
            "ab".repeat(32)
        );
        let marks = AtomicU8::new(0);
        for broken in [huge.as_str(), "壊れた値", "$pbkdf2-sha256$i=10$00"] {
            counts::take();
            assert!(!check_using("x", broken, PBKDF2, &marks), "{broken}");
            assert_eq!(counts::take(), (1, 0, 0), "{broken}");
        }
        assert_eq!(
            marks.load(Ordering::Relaxed),
            0,
            "読めない値では印を立てない"
        );
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
        counts::take();
        waste_time_using(&long, PBKDF2, &AtomicU8::new(0));
        assert_eq!(counts::take(), (0, 0, 0), "長すぎる入力は計算しない");
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

    // ---- 方式（HASH_DRIVER）と Argon2id の設定 ----

    #[test]
    fn 方式の名前は空白を落として小文字で比べる() {
        assert_eq!(parse_driver("pbkdf2"), Some(Driver::Pbkdf2));
        assert_eq!(parse_driver(" Argon2id "), Some(Driver::Argon2id));
        assert_eq!(parse_driver("ARGON2ID"), Some(Driver::Argon2id));
        assert_eq!(parse_driver("bcrypt"), None);
        assert_eq!(parse_driver("argon"), None, "argon2i は無い");
    }

    #[test]
    fn 知らない方式は_pbkdf2_を使う() {
        let choice = resolve_driver("bcrypt");
        assert_eq!(choice.driver, Driver::Pbkdf2);
        assert!(!choice.missing_flag);
    }

    #[test]
    fn argon2id_を頼むとフラグの有無で方式が決まる() {
        let choice = resolve_driver("argon2id");
        if cfg!(feature = "argon2") {
            assert_eq!(choice.driver, Driver::Argon2id);
            assert!(!choice.missing_flag);
        } else {
            assert_eq!(
                choice.driver,
                Driver::Pbkdf2,
                "フラグが無ければ PBKDF2 で作る"
            );
            assert!(choice.missing_flag);
        }
    }

    #[test]
    fn argon2id_の既定値は_laravel_と同じ() {
        assert_eq!(DEFAULT_ARGON_MEMORY, 65_536);
        assert_eq!(DEFAULT_ARGON_TIME, 4);
        assert_eq!(DEFAULT_ARGON_THREADS, 1);
    }

    #[test]
    fn argon2id_の設定は範囲に寄せる() {
        let config = |memory, time, threads| ArgonConfig {
            memory,
            time,
            threads,
        };
        assert_eq!(config(65_536, 4, 1).clamped(), config(65_536, 4, 1));
        assert_eq!(config(0, 0, 0).clamped(), config(8, 1, 1));
        assert_eq!(
            config(u32::MAX, u32::MAX, u32::MAX).clamped(),
            config(MAX_ARGON_MEMORY, MAX_ARGON_TIME, MAX_ARGON_THREADS)
        );
        // メモリの下限は並列の数の 8 倍。
        assert_eq!(config(8, 1, 4).clamped(), config(32, 1, 4));
    }

    #[test]
    fn テストでは_argon2id_の設定も下げる() {
        install_test_iterations();
        let given = ["ARGON_MEMORY", "ARGON_TIME", "ARGON_THREADS"]
            .iter()
            .any(|key| env_given(std::env::var(key).ok().as_deref()));
        if !given {
            assert_eq!(argon_config(), TEST_ARGON);
        }
    }

    // ---- 7.1 の表：各呼ばれ方が空回しを呼ぶか ----

    #[test]
    fn 方式が_pbkdf2_で何も見ていなければ_pbkdf2_だけ空回しする() {
        install_test_iterations();
        let marks = AtomicU8::new(0);
        counts::take();
        waste_time_using("x", PBKDF2, &marks);
        assert_eq!(counts::take(), (1, 0, 0));

        // PBKDF2 の値の照合は、もう片方（Argon2id）を空回ししない。
        let stored = make_pbkdf2("x", FAST);
        assert!(check_using("x", &stored, PBKDF2, &marks));
        assert_eq!(counts::take(), (0, 0, 0));
        assert_eq!(marks.load(Ordering::Relaxed), SEEN_PBKDF2, "印を立てる");
    }

    #[test]
    fn 方式が_pbkdf2_のとき作るのも_pbkdf2() {
        install_test_iterations();
        let stored = make_using("x", PBKDF2);
        assert!(stored.starts_with(PBKDF2_PREFIX), "{stored}");
        assert!(!needs_rehash_using(&stored, PBKDF2));
    }

    #[test]
    fn 方式が_argon2id_でも_make_with_は_pbkdf2() {
        install_test_iterations();
        // 回数を指定する関数なので、方式によらず PBKDF2。
        let stored = Hash::make_with("x", FAST);
        assert!(stored.starts_with("$pbkdf2-sha256$i=50$"), "{stored}");
    }

    #[cfg(feature = "argon2")]
    #[test]
    fn php_の_password_hash_で作った値を照合できる() {
        install_test_iterations();
        let marks = AtomicU8::new(0);
        assert!(check_using("ひみつのパスワード", PHP_ARGON, ARGON, &marks));
        assert!(!check_using("ちがうパスワード", PHP_ARGON, ARGON, &marks));
        assert!(check_using("ひみつのパスワード", PHP_ARGON, PBKDF2, &marks));
    }

    #[cfg(feature = "argon2")]
    #[test]
    fn 方式が_argon2id_なら_argon2id_で作って照合できる() {
        install_test_iterations();
        let stored = make_using("ひみつ", ARGON);
        assert!(
            stored.starts_with("$argon2id$v=19$m=8,t=1,p=1$"),
            "{stored}"
        );
        let marks = AtomicU8::new(0);
        assert!(check_using("ひみつ", &stored, ARGON, &marks));
        assert!(!check_using("ちがう", &stored, ARGON, &marks));
        assert_eq!(marks.load(Ordering::Relaxed), SEEN_ARGON2ID, "印を立てる");
        // 塩は毎回変わる。
        assert_ne!(stored, make_using("ひみつ", ARGON));
        // 方式が PBKDF2 になっても照合できる。
        assert!(check_using("ひみつ", &stored, PBKDF2, &marks));
    }

    #[cfg(feature = "argon2")]
    #[test]
    fn 作り直しの判定は方式ごとに決まる() {
        install_test_iterations();
        let pbkdf2 = make_pbkdf2("x", iterations());
        let argon = make_using("x", ARGON);

        // 方式が PBKDF2：PBKDF2 は今のまま。Argon2id は作り直さない。
        assert!(!needs_rehash_using(&pbkdf2, PBKDF2));
        assert!(
            !needs_rehash_using(&argon, PBKDF2),
            "強い方式から弱い方式へは移らない"
        );
        assert!(needs_rehash_using("$2y$10$abc", PBKDF2));

        // 方式が Argon2id：PBKDF2 は作り直す。設定どおりの Argon2id は作り直さない。
        assert!(needs_rehash_using(&pbkdf2, ARGON), "ログインのついでに移る");
        assert!(!needs_rehash_using(&argon, ARGON));
        assert!(needs_rehash_using("$2y$10$abc", ARGON));
        assert!(needs_rehash_using("$argon2id$壊れた値", ARGON));

        // 設定より小さい m・t は作り直す。p は見ない。
        let stronger = Policy {
            driver: Driver::Argon2id,
            argon: ArgonConfig {
                memory: 16,
                time: 1,
                threads: 1,
            },
        };
        assert!(needs_rehash_using(&argon, stronger), "m が小さい");
        let longer = Policy {
            driver: Driver::Argon2id,
            argon: ArgonConfig {
                memory: 8,
                time: 2,
                threads: 1,
            },
        };
        assert!(needs_rehash_using(&argon, longer), "t が小さい");
        let wider = format!("$argon2id$v=19$m=16,t=1,p=2${}", &PHP_ARGON[27..]);
        assert!(!needs_rehash_using(&wider, ARGON), "p は見ない");

        // 上限を超える値は作り直す。
        let over = PHP_ARGON.replace("m=8,", "m=2000000,");
        assert!(needs_rehash_using(&over, ARGON));
        assert!(needs_rehash_using(&over, PBKDF2), "読めない値と同じ");
    }

    #[cfg(feature = "argon2")]
    #[test]
    fn 上限を超える_argon2id_の値は計算せずに偽になる() {
        install_test_iterations();
        let marks = AtomicU8::new(0);
        for over in [
            PHP_ARGON.replace("m=8,", &format!("m={},", MAX_ARGON_MEMORY + 1)),
            PHP_ARGON.replace("t=1,", &format!("t={},", MAX_ARGON_TIME + 1)),
            PHP_ARGON.replace(
                "m=8,t=1,p=1",
                &format!("m={},t=1,p={}", 8 * 17, MAX_ARGON_THREADS + 1),
            ),
        ] {
            counts::take();
            let started = std::time::Instant::now();
            assert!(
                !check_using("ひみつのパスワード", &over, ARGON, &marks),
                "{over}"
            );
            assert!(started.elapsed().as_secs() < 1, "すぐ返る");
            // 保存値では計算しない。空回し（方式の Argon2id）はする。
            assert_eq!(counts::take(), (0, 1, 0), "{over}");
        }
        assert_eq!(
            marks.load(Ordering::Relaxed),
            0,
            "読めない値では印を立てない"
        );
    }

    #[cfg(feature = "argon2")]
    #[test]
    fn argon2id_でない_argon2_の値は照合しない() {
        install_test_iterations();
        let marks = AtomicU8::new(0);
        for other in [
            PHP_ARGON.replacen("argon2id", "argon2i", 1),
            PHP_ARGON.replace("v=19", "v=16"),
            "$argon2id$v=19$m=8,t=1,p=1".to_string(),
            "$argon2id$壊れた値".to_string(),
        ] {
            counts::take();
            assert!(
                !check_using("ひみつのパスワード", &other, ARGON, &marks),
                "{other}"
            );
            assert_eq!(counts::take(), (0, 1, 0), "{other}");
        }
    }

    #[cfg(feature = "argon2")]
    #[test]
    fn 空回しは方式と見た印で決まる() {
        install_test_iterations();

        // 方式が Argon2id で何も見ていない：Argon2id だけ。
        let marks = AtomicU8::new(0);
        counts::take();
        waste_time_using("x", ARGON, &marks);
        assert_eq!(counts::take(), (0, 1, 0), "いない利用者");
        assert!(!check_using("x", "$2y$10$abc", ARGON, &marks));
        assert_eq!(counts::take(), (0, 1, 0), "読めない値");
        assert!(check_using("ひみつのパスワード", PHP_ARGON, ARGON, &marks));
        assert_eq!(
            counts::take(),
            (0, 0, 1),
            "Argon2id の値：もう片方は回さない"
        );

        // PBKDF2 の値を一度照合すると、PBKDF2 の印が立つ。
        let pbkdf2 = make_pbkdf2("x", FAST);
        assert!(check_using("x", &pbkdf2, ARGON, &marks));
        assert_eq!(counts::take(), (0, 1, 0), "PBKDF2 の値：Argon2id を空回し");
        assert_eq!(marks.load(Ordering::Relaxed), SEEN_PBKDF2 | SEEN_ARGON2ID);

        // 両方の印が立ったら、どの呼ばれ方でも両方の時間をかける。
        waste_time_using("x", ARGON, &marks);
        assert_eq!(counts::take(), (1, 1, 0), "いない利用者");
        assert!(check_using("ひみつのパスワード", PHP_ARGON, ARGON, &marks));
        assert_eq!(counts::take(), (1, 0, 1), "Argon2id の値：PBKDF2 を空回し");
        assert!(!check_using("x", "壊れた値", ARGON, &marks));
        assert_eq!(counts::take(), (1, 1, 0), "読めない値：両方");

        // 方式が PBKDF2 で、Argon2id の値を見たあと。
        let marks = AtomicU8::new(SEEN_ARGON2ID);
        waste_time_using("x", PBKDF2, &marks);
        assert_eq!(counts::take(), (1, 1, 0), "いない利用者");
        assert!(check_using("x", &pbkdf2, PBKDF2, &marks));
        assert_eq!(counts::take(), (0, 1, 0), "PBKDF2 の値：Argon2id を空回し");
    }

    #[cfg(not(feature = "argon2"))]
    #[test]
    fn フラグが無ければ_argon2id_の値は偽でエラーは1回だけ() {
        install_test_iterations();
        let marks = AtomicU8::new(0);
        counts::take();
        assert!(!check_using(
            "ひみつのパスワード",
            PHP_ARGON,
            PBKDF2,
            &marks
        ));
        assert!(!check_using(
            "ひみつのパスワード",
            PHP_ARGON,
            PBKDF2,
            &marks
        ));
        assert_eq!(
            MISSING_FLAG_REPORTS.load(Ordering::Relaxed),
            1,
            "エラーは 1 回だけ"
        );
        // 読めない値と同じく空回しする。
        assert_eq!(counts::take(), (2, 0, 0));
        assert_eq!(marks.load(Ordering::Relaxed), 0);

        // 作り直しの判定は「読めない値」と同じ。
        assert!(needs_rehash_using(PHP_ARGON, PBKDF2));
        assert!(needs_rehash_using(PHP_ARGON, ARGON));
        // 方式を Argon2id にしても、フラグが無ければ PBKDF2 で作る。
        assert!(make_using("x", ARGON).starts_with(PBKDF2_PREFIX));
    }

    // ---- 7.2 計算の同時数 ----

    #[test]
    fn 同時数の既定は1以上() {
        assert!(max_concurrency() >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 上限が1なら2件目は1件目が終わるまで待つ() {
        use std::sync::{Arc, Condvar, Mutex};

        let semaphore = Semaphore::new(1);
        let started = Arc::new(AtomicU32::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));

        // どちらが先に permit を取っても、先の 1 件は門が開くまで終わらない。
        let task = |n: u32| {
            let started = Arc::clone(&started);
            let gate = Arc::clone(&gate);
            move || {
                started.fetch_add(1, Ordering::SeqCst);
                let (lock, opened) = &*gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = opened.wait(open).unwrap();
                }
                n
            }
        };
        let first = run_hashing_with(&semaphore, task(1));
        let second = run_hashing_with(&semaphore, task(2));
        let watch = async {
            while started.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert_eq!(started.load(Ordering::SeqCst), 1, "2 件目は permit を待つ");
            assert_eq!(semaphore.available_permits(), 0);
            let (lock, opened) = &*gate;
            *lock.lock().unwrap() = true;
            opened.notify_all();
        };
        let (a, b, ()) = tokio::join!(first, second, watch);
        assert_eq!((a.unwrap(), b.unwrap()), (1, 2));
        assert_eq!(started.load(Ordering::SeqCst), 2);
        assert_eq!(semaphore.available_permits(), 1, "終わったら返す");
    }
}
