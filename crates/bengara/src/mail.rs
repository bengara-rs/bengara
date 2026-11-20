//! メール。文面を組み立てて送ります。
//!
//! ```ignore
//! Mail::to("alice@example.com")
//!     .subject("ようこそ")
//!     .text("登録ありがとうございます。")
//!     .send()
//!     .await?;
//! ```
//!
//! **送り先は3つです。** `MAIL_MAILER`（Laravel と同じ名前）で選びます。
//! 空なら `MAIL_DRIVER` を見ます。知らない名前は送るときにエラーになります。
//!
//! | 送り先 | 何をするか | 要るもの |
//! |---|---|---|
//! | `log`（既定） | `storage/logs/mail.log` に書き出す | 無し |
//! | `array`（別名 `memory`） | プロセスのメモリに溜める（テスト用） | 無し |
//! | `smtp` | SMTP サーバへ送る | Cargo の機能フラグ `mail` |
//!
//! `smtp` は既定では入りません。フラグが無いままでもコンパイルはできて、
//! 送ろうとしたときに直し方を書いたエラーになります。
//! `features` に `"mail"` を**足します**（DB のフラグは残します）。
//!
//! ```toml
//! bengara = { version = "0.1", features = ["sqlite", "mail"] }
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{Error, Result};

/// SMTP の送り口。`mail` フラグを付けたときだけ入ります。
#[cfg(feature = "mail")]
mod smtp;

/// メールの設定。`config/mail.rs` が返します。
///
/// `host` から下は `smtp` のときだけ使います。
#[derive(Debug, Clone)]
pub struct MailConfig {
    /// 送り先の名前（`log` / `array` / `smtp`）。`memory` は `array` の別名です。
    ///
    /// 大文字小文字と前後の空白は見ません。知らない名前は `log` に落とさず、
    /// 送るときにエラーにします。
    pub driver: String,
    /// 差出人（`.from(..)` を書かなかったときに使う）。
    pub from: String,
    /// 差出人の名前。
    pub from_name: String,
    /// SMTP サーバの名前。
    pub host: String,
    /// SMTP サーバのポート。**0 のときは `encryption` から決めます**
    /// （`tls` は 465、`starttls` は 587、`none` は 25）。
    pub port: u16,
    /// 暗号化の仕方（`tls` / `starttls` / `none`）。
    ///
    /// `tls`（`ssl`）は、ポートが 465 か 0 のときだけ「つないだ直後から暗号化」
    /// します。ほかのポート（587 など）では `STARTTLS` を使います（Laravel と同じ）。
    /// `smtps` はポートに関係なく、つないだ直後から暗号化します。
    pub encryption: String,
    /// SMTP の利用者名。空のときは認証しません。
    pub username: String,
    /// SMTP のパスワード。
    pub password: String,
    /// `EHLO` で名乗る名前。空のときは lettre の既定で、`[127.0.0.1]` と名乗ります。
    ///
    /// `localhost` ではありません。lettre は `hostname` 機能を入れないかぎり
    /// IPv4 のアドレス表記（`ClientId::Ipv4(127.0.0.1)`）を使います。
    /// 名乗りを確かめるサーバには断られることがあるので、そのときは
    /// 自分のホスト名（`app.example.com` など）を書いてください。
    pub ehlo_name: String,
    /// 返事を待つ上限（秒）。
    pub timeout: u64,
}

impl Default for MailConfig {
    /// 環境変数から組み立てます。
    ///
    /// | 環境変数            | 既定値                 |
    /// |---------------------|------------------------|
    /// | `MAIL_MAILER`       | 空なら `MAIL_DRIVER`   |
    /// | `MAIL_DRIVER`       | `log`                  |
    /// | `MAIL_FROM_ADDRESS` | 空なら `MAIL_FROM`（Laravel の名前が先） |
    /// | `MAIL_FROM`         | `noreply@example.com`  |
    /// | `MAIL_FROM_NAME`    | `bengara`              |
    /// | `MAIL_HOST`         | `127.0.0.1`            |
    /// | `MAIL_PORT`         | 0（暗号化から決める）  |
    /// | `MAIL_SCHEME`       | 空（Laravel 11 の `smtp` / `smtps`。書けば `MAIL_ENCRYPTION` より先） |
    /// | `MAIL_ENCRYPTION`   | `starttls`（空も同じ。暗号化しないなら `none`） |
    /// | `MAIL_USERNAME`     | 空（認証しない。`null` も空） |
    /// | `MAIL_PASSWORD`     | 空（`null` も空）      |
    /// | `MAIL_EHLO_NAME`    | 空                     |
    /// | `MAIL_TIMEOUT`      | 10（秒）               |
    fn default() -> Self {
        let port = crate::env::<u16>("MAIL_PORT", 0u16);
        Self {
            driver: driver_from_env(),
            from: from_address(
                &crate::env::<String>("MAIL_FROM_ADDRESS", ""),
                &crate::env::<String>("MAIL_FROM", ""),
            ),
            from_name: crate::env("MAIL_FROM_NAME", "bengara"),
            host: crate::env("MAIL_HOST", "127.0.0.1"),
            port,
            encryption: encryption_from(
                &crate::env::<String>("MAIL_SCHEME", ""),
                &crate::env::<String>("MAIL_ENCRYPTION", "starttls"),
                port,
            ),
            username: null_as_empty(crate::env("MAIL_USERNAME", "")),
            password: null_as_empty(crate::env("MAIL_PASSWORD", "")),
            ehlo_name: crate::env("MAIL_EHLO_NAME", ""),
            timeout: crate::env::<u64>("MAIL_TIMEOUT", 10u64),
        }
    }
}

/// Laravel の `.env` の `null`（大文字小文字は見ない）を空にする。
///
/// Laravel の `.env.example` は `MAIL_USERNAME=null` と書きます。PHP の `env()` は
/// これを null に読みますが、そのまま使うと「`null` という利用者名」で認証していました。
fn null_as_empty(value: String) -> String {
    if value.trim().eq_ignore_ascii_case("null") {
        String::new()
    } else {
        value
    }
}

/// 差出人のアドレスを決める。`MAIL_FROM_ADDRESS`（Laravel の名前）が先、空なら `MAIL_FROM`。
///
/// どちらも空（か `null`）なら `noreply@example.com` です。
fn from_address(laravel: &str, legacy: &str) -> String {
    [laravel, legacy]
        .into_iter()
        .map(str::trim)
        .find(|value| !value.is_empty() && !value.eq_ignore_ascii_case("null"))
        .unwrap_or("noreply@example.com")
        .to_string()
}

/// 暗号化の仕方を決める。`MAIL_SCHEME`（Laravel 11）を書いていれば先に見ます。
///
/// Laravel 11 は `MAIL_ENCRYPTION` をやめて `MAIL_SCHEME` を読みます。書いてあれば
/// `MAIL_ENCRYPTION` より先です（Laravel の `MailManager` と同じ順番）。
///
/// | `MAIL_SCHEME`   | 使う暗号化 |
/// |-----------------|------------|
/// | 空・`null`      | `MAIL_ENCRYPTION` のまま（判断 222 の `null` = 暗号化なし も同じ） |
/// | `smtps`         | `smtps`（ポートに関係なく最初から暗号化） |
/// | `smtp`          | 465 番なら `smtps`。ほかは `starttls`。ただし `MAIL_ENCRYPTION` が `none` / `null` / `false` なら暗号化しない |
/// | ほかの語        | 警告を出して `MAIL_ENCRYPTION` のまま |
///
/// `smtp` で `MAIL_ENCRYPTION` の「暗号化しない」を残すのは、手元の受け口
/// （Mailpit など）が `STARTTLS` を持たないためです。Laravel（Symfony Mailer）は
/// サーバが `STARTTLS` を持たなければ平文で送りますが、bengara は切り替えを必ず求めます。
fn encryption_from(scheme: &str, encryption: &str, port: u16) -> String {
    let scheme = scheme.trim().to_ascii_lowercase();
    match scheme.as_str() {
        "" | "null" => encryption.to_string(),
        "smtps" => "smtps".to_string(),
        "smtp" => {
            let plain = matches!(
                encryption.trim().to_ascii_lowercase().as_str(),
                "none" | "null" | "false"
            );
            if port == 465 {
                "smtps".to_string()
            } else if plain {
                "none".to_string()
            } else {
                "starttls".to_string()
            }
        }
        other => {
            tracing::warn!(
                "MAIL_SCHEME `{other}` は知りません（smtp か smtps）。MAIL_ENCRYPTION を使います"
            );
            encryption.to_string()
        }
    }
}

/// 送り先の名前を環境変数から読む。`MAIL_MAILER` が先、空なら `MAIL_DRIVER`。
///
/// Laravel は `MAIL_MAILER` です。`MAIL_DRIVER` は古い Laravel（6 まで）の
/// 名前で、bengara は最初こちらだけを読んでいました。Laravel の `.env` を
/// 持ってくると `MAIL_MAILER=smtp` が効かず、黙って `log` に書き出していました。
fn driver_from_env() -> String {
    driver_from(
        std::env::var("MAIL_MAILER").ok().as_deref(),
        std::env::var("MAIL_DRIVER").ok().as_deref(),
    )
}

/// [`driver_from_env`] の中身（環境変数を触らずに確かめるため）。
fn driver_from(mailer: Option<&str>, driver: Option<&str>) -> String {
    [mailer, driver]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or("log")
        .to_string()
}

/// 送り先を環境変数で決めてあるか（`MAIL_MAILER` か `MAIL_DRIVER` が空でない）。
fn driver_set_in_env() -> bool {
    driver_set(
        std::env::var("MAIL_MAILER").ok().as_deref(),
        std::env::var("MAIL_DRIVER").ok().as_deref(),
    )
}

/// [`driver_set_in_env`] の中身。
fn driver_set(mailer: Option<&str>, driver: Option<&str>) -> bool {
    [mailer, driver]
        .into_iter()
        .flatten()
        .any(|name| !name.trim().is_empty())
}

/// いまの送り先の名前（`about` の表示用）。
pub(crate) fn driver_label() -> String {
    config().driver.trim().to_ascii_lowercase()
}

/// いまの設定。
///
/// `try_config` は `&'static` を返すので**借りて回します**。クローンすると
/// `Mail::to` 1 回につき `MailConfig` のコピーが走ります。
fn config() -> &'static MailConfig {
    static FALLBACK: OnceLock<MailConfig> = OnceLock::new();
    crate::try_config::<MailConfig>().unwrap_or_else(|| FALLBACK.get_or_init(MailConfig::default))
}

/// 組み立てたメール1通。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Message {
    /// 宛先。
    pub to: Vec<String>,
    /// 写しの宛先。
    pub cc: Vec<String>,
    /// 隠した写しの宛先。
    pub bcc: Vec<String>,
    /// 差出人。
    pub from: String,
    /// 返信先。
    pub reply_to: Option<String>,
    /// 件名。
    pub subject: String,
    /// 本文（文字だけ）。
    pub text: Option<String>,
    /// 本文（HTML）。
    pub html: Option<String>,
}

impl Message {
    /// ログに書き出すときの形。
    pub fn to_log(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("---- {} ----\n", crate::database::now()));
        out.push_str(&format!("From: {}\n", self.from));
        // 空白だけの宛先は書かない。`smtp` が飛ばすのと同じにして、
        // 送り先を切り替えても宛先の見え方が変わらないようにする。
        out.push_str(&format!("To: {}\n", join_addresses(&self.to)));
        let cc = join_addresses(&self.cc);
        if !cc.is_empty() {
            out.push_str(&format!("Cc: {cc}\n"));
        }
        let bcc = join_addresses(&self.bcc);
        if !bcc.is_empty() {
            out.push_str(&format!("Bcc: {bcc}\n"));
        }
        if let Some(reply_to) = &self.reply_to {
            out.push_str(&format!("Reply-To: {reply_to}\n"));
        }
        out.push_str(&format!("Subject: {}\n\n", self.subject));
        if let Some(text) = &self.text {
            out.push_str(text);
            out.push('\n');
        }
        if let Some(html) = &self.html {
            out.push_str("\n---- HTML ----\n");
            out.push_str(html);
            out.push('\n');
        }
        out
    }
}

/// 宛先を `, ` でつなぐ。**空白だけのものは飛ばします。**
fn join_addresses(addresses: &[String]) -> String {
    addresses
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// メールの送り先。自分で作れば差し替えられます。
pub trait Mailer: Send + Sync + 'static {
    /// 1通送る。
    fn send(&self, message: &Message) -> Result<()>;
}

/// ファイルに書き出す送り先（既定）。
pub struct LogMailer;

impl Mailer for LogMailer {
    fn send(&self, message: &Message) -> Result<()> {
        use std::io::Write as _;

        let dir = crate::paths::storage_path("logs");
        if !dir.is_dir() {
            // storage/ が無いときは、落とさずログだけに出す（決定記録 #023）。
            tracing::info!(
                "メール（書き出し先が無いのでログに出します）:\n{}",
                message.to_log()
            );
            return Ok(());
        }
        let path = dir.join("mail.log");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(message.to_log().as_bytes())?;
        tracing::info!("メールを {} に書き出しました", path.display());
        Ok(())
    }
}

/// プロセスのメモリに溜める送り先（テスト用）。
pub struct ArrayMailer;

static SENT: OnceLock<Mutex<Vec<Message>>> = OnceLock::new();

fn sent() -> &'static Mutex<Vec<Message>> {
    SENT.get_or_init(|| Mutex::new(Vec::new()))
}

impl Mailer for ArrayMailer {
    fn send(&self, message: &Message) -> Result<()> {
        sent()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(message.clone());
        Ok(())
    }
}

static MAILER: OnceLock<Box<dyn Mailer>> = OnceLock::new();

/// 送り先を差し替える。`bootstrap/app.rs` かテストから呼びます。
///
/// 1回目だけ効きます。2回目以降は何もせず、警告を出します。
pub fn install_mailer(mailer: Box<dyn Mailer>) {
    if MAILER.set(mailer).is_err() {
        tracing::warn!("メールの送り先は既に決まっています。この install_mailer は効きません");
    }
}

/// テストのときに `array` を使う。`#[bengara::test]` が呼びます。
///
/// `MAIL_MAILER` か `MAIL_DRIVER` に**空でない**値があれば、そちらに従います。
/// 以前は `MAIL_DRIVER=`（空）でも「決めてある」と見て、テストが
/// `log` に書き出していました（`env()` は空を既定値として扱うのに）。
pub(crate) fn install_test_mailer() {
    if driver_set_in_env() {
        return;
    }
    install_mailer(Box::new(ArrayMailer));
}

/// 送れない送り先。理由だけを持ちます。
///
/// 設定が足りなくても**起動そのものは止めません。** メールを送ろうとした
/// ときに、直し方を書いたエラーを返します。起動時に落とすと、メールを
/// 使わない画面まで開けなくなるためです。
struct UnavailableMailer {
    reason: String,
}

impl Mailer for UnavailableMailer {
    fn send(&self, _message: &Message) -> Result<()> {
        Err(Error::msg(self.reason.clone()))
    }
}

/// 名前から送り先を作る。
///
/// 大文字小文字と前後の空白は見ません（`SMTP` も `smtp`）。空は既定の `log` です。
///
/// **知らない名前は `log` に落としません。** [`UnavailableMailer`] にして、
/// 送ろうとしたときにエラーにします（機能フラグの無い `smtp` と同じ扱い）。
/// 以前は `log` に落としていたので、`MAIL_MAILER=ses` のような打ち間違いや
/// 未対応の名前で、送れたつもりのまま 1 通も届きませんでした。
///
/// `mailer()` から切り出してあります。`MAILER` は 1 回しか決まらないので、
/// 中に書くと選び方をテストから確かめられません。
fn make_mailer(driver: &str) -> Box<dyn Mailer> {
    let name = driver.trim().to_ascii_lowercase();
    match name.as_str() {
        "array" | "memory" => Box::new(ArrayMailer) as Box<dyn Mailer>,
        "log" | "" => Box::new(LogMailer),
        "smtp" => make_smtp_mailer(),
        other => {
            let reason = unknown_driver_message(other);
            tracing::warn!("{reason}");
            Box::new(UnavailableMailer { reason })
        }
    }
}

/// 知らない送り先の名前の説明文。
fn unknown_driver_message(name: &str) -> String {
    format!(
        "メールの送り先 `{name}` は知りません（MAIL_MAILER / MAIL_DRIVER）。\
         使えるのは log・array（memory）・smtp です"
    )
}

/// 機能フラグ `mail` が無いときの案内。
///
/// **「足してください」と言います。** 以前は「`features = ["mail"]` に
/// してください」で、そのとおりに書くと `sqlite` などが消えて DB が
/// 使えなくなりました。
#[cfg(not(feature = "mail"))]
const MAIL_FEATURE_HINT: &str = "SMTP で送るには、Cargo.toml の bengara の features に \
    `\"mail\"` を足してください（例: `features = [\"sqlite\", \"mail\"]`）";

/// SMTP の送り先を作る。
///
/// 機能フラグ `mail` が無いときと、設定が足りないときは、どちらも
/// [`UnavailableMailer`] になります。**黙って log に落としません。**
/// SMTP を指定した人は送れたつもりになってしまうためです。
fn make_smtp_mailer() -> Box<dyn Mailer> {
    #[cfg(feature = "mail")]
    {
        match smtp::SmtpMailer::new(config()) {
            Ok(mailer) => Box::new(mailer) as Box<dyn Mailer>,
            Err(error) => Box::new(UnavailableMailer {
                reason: error.to_string(),
            }),
        }
    }
    #[cfg(not(feature = "mail"))]
    {
        Box::new(UnavailableMailer {
            reason: MAIL_FEATURE_HINT.to_string(),
        })
    }
}

fn mailer() -> &'static dyn Mailer {
    MAILER
        .get_or_init(|| make_mailer(&config().driver))
        .as_ref()
}

/// メールの入口。
pub struct Mail;

impl Mail {
    /// 宛先を決めて組み立てを始める。
    pub fn to(address: impl Into<String>) -> Builder {
        let config = config();
        Builder {
            message: Message {
                to: vec![address.into()],
                from: from_header(&config.from_name, &config.from),
                ..Message::default()
            },
        }
    }

    /// 送ったメールの一覧（`array` のときだけ溜まります）。
    pub fn sent() -> Vec<Message> {
        sent().lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 溜めた一覧を空にする。テストの先頭で呼びます。
    pub fn clear_sent() {
        sent().lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// メールの組み立て。
pub struct Builder {
    message: Message,
}

impl Builder {
    /// 宛先を足す。
    pub fn also_to(mut self, address: impl Into<String>) -> Self {
        self.message.to.push(address.into());
        self
    }

    /// 写しの宛先。
    pub fn cc(mut self, address: impl Into<String>) -> Self {
        self.message.cc.push(address.into());
        self
    }

    /// 隠した写しの宛先。
    pub fn bcc(mut self, address: impl Into<String>) -> Self {
        self.message.bcc.push(address.into());
        self
    }

    /// 返信先。
    pub fn reply_to(mut self, address: impl Into<String>) -> Self {
        self.message.reply_to = Some(address.into());
        self
    }

    /// 差出人を変える。
    pub fn from(mut self, address: impl Into<String>) -> Self {
        self.message.from = address.into();
        self
    }

    /// 件名。
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.message.subject = subject.into();
        self
    }

    /// 本文（文字だけ）。
    pub fn text(mut self, body: impl Into<String>) -> Self {
        self.message.text = Some(body.into());
        self
    }

    /// 本文（HTML）。
    pub fn html(mut self, body: impl Into<String>) -> Self {
        self.message.html = Some(body.into());
        self
    }

    /// 組み立てた中身を見る（送らない）。
    pub fn message(&self) -> &Message {
        &self.message
    }

    /// 送る。
    pub async fn send(self) -> Result<()> {
        send_with(mailer(), self.message).await
    }
}

/// 差出人の欄（`"名前" <アドレス>`）を組み立てる。
///
/// 名前は**引用符で囲みます。** 囲まずに `名前 <アドレス>` と書いていたころは、
/// `MAIL_FROM_NAME`（既定はアプリ名）に `,` や `"` が入ると SMTP 側の読み取りが
/// 「From のアドレスが読めません」で落ち、1 通も送れませんでした。
/// 引用符の中では `\` と `"` を `\` で逃がします（RFC 5322 の quoted-string）。
///
/// アドレスに `名前 <addr>` の形を書いたときは、`<>` の中だけを使います
/// （[`bare_address`]）。そのまま包むと `"名前" <名前 <addr>>` になり、送れませんでした。
fn from_header(name: &str, address: &str) -> String {
    let address = bare_address(address);
    let name = name.trim();
    if name.is_empty() {
        return address.to_string();
    }
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\" <{address}>")
}

/// 差出人のアドレスに `名前 <addr>` を書いたとき、`<>` の中だけを返す。
///
/// Laravel の `MAIL_FROM_ADDRESS` はアドレスだけを書く欄です。名前は `MAIL_FROM_NAME`
/// に書きます。エラーにせずアドレスを取り出し、直し方を警告で 1 回だけ知らせます
/// （1 通ごとに出すとログが埋まるため）。
/// `<` があっても `<..>` の形になっていなければ、そのまま返します。
fn bare_address(address: &str) -> &str {
    let trimmed = address.trim();
    if !trimmed.contains('<') {
        return trimmed;
    }
    let inner = trimmed
        .strip_suffix('>')
        .and_then(|rest| rest.rfind('<').map(|at| rest[at + 1..].trim()))
        .filter(|inner| !inner.is_empty() && !inner.contains(['<', '>']));
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "差出人のアドレス `{trimmed}` に名前が入っています。MAIL_FROM_ADDRESS（MAIL_FROM）には\
             アドレスだけを書き、名前は MAIL_FROM_NAME に書いてください{}",
            if inner.is_some() {
                "（`<>` の中のアドレスだけを使います）"
            } else {
                "（形が読めないので、そのまま使います）"
            }
        );
    }
    inner.unwrap_or(trimmed)
}

/// 送れる形になっているか。
///
/// 確かめるのは次の**3 つ**です。
///
/// | 断るもの | 理由 |
/// |---|---|
/// | 宛先が空 | 送り先によっては黙って捨てられ、送れたつもりになる |
/// | 本文が空 | 同じ |
/// | 宛先・差出人・件名に改行 | `log` の送り先はヘッダーを 1 行ずつ書くので、偽のヘッダー行を作れた |
///
/// 改行の検査は `to` / `cc` / `bcc` / `from` / `reply_to` / `subject` に掛けます。
fn check(message: &Message) -> Result<()> {
    if message.to.iter().all(|a| a.trim().is_empty()) {
        return Err(Error::msg("メールの宛先がありません"));
    }
    if message.text.is_none() && message.html.is_none() {
        return Err(Error::msg("メールの本文がありません"));
    }
    let header_fields = message
        .to
        .iter()
        .chain(&message.cc)
        .chain(&message.bcc)
        .chain(std::iter::once(&message.from))
        .chain(message.reply_to.iter())
        .chain(std::iter::once(&message.subject));
    for field in header_fields {
        if field.contains(['\r', '\n']) {
            return Err(Error::msg(
                "メールの宛先・差出人・件名に改行は入れられません",
            ));
        }
    }
    Ok(())
}

/// 送り先を指定して送る（[`Builder::send`] の本体）。
///
/// 送り先を引数で受け取ります。`install_mailer` は 1 回しか効かないので、
/// `mailer()` 越しだと検査と送りをテストから確かめられません。
pub(crate) async fn send_with(mailer: &'static dyn Mailer, message: Message) -> Result<()> {
    check(&message)?;
    crate::support::blocking(move || mailer.send(&message)).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 既定の設定() {
        let config = MailConfig::default();
        assert_eq!(config.driver, "log");
        assert!(config.from.contains('@'));
        // SMTP の値は入っているが、driver が log なので使われない。
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.encryption, "starttls");
        assert_eq!(config.port, 0, "0 は「暗号化から決める」の合図");
        assert!(config.username.is_empty(), "既定では認証しない");
        assert_eq!(config.timeout, 10);
    }

    #[test]
    fn 組み立てた中身が入る() {
        let builder = Mail::to("a@example.com")
            .also_to("b@example.com")
            .cc("c@example.com")
            .bcc("d@example.com")
            .reply_to("r@example.com")
            .from("me@example.com")
            .subject("件名")
            .text("本文")
            .html("<p>本文</p>");
        let message = builder.message();

        assert_eq!(message.to, ["a@example.com", "b@example.com"]);
        assert_eq!(message.cc, ["c@example.com"]);
        assert_eq!(message.bcc, ["d@example.com"]);
        assert_eq!(message.reply_to.as_deref(), Some("r@example.com"));
        assert_eq!(message.from, "me@example.com");
        assert_eq!(message.subject, "件名");
        assert_eq!(message.text.as_deref(), Some("本文"));
        assert_eq!(message.html.as_deref(), Some("<p>本文</p>"));
    }

    #[test]
    fn ログの形に宛先と件名が出る() {
        let log = Mail::to("a@example.com")
            .subject("件名")
            .text("本文")
            .message()
            .to_log();
        assert!(log.contains("To: a@example.com"));
        assert!(log.contains("Subject: 件名"));
        assert!(log.contains("本文"));
    }

    #[test]
    fn ログの形は空白だけの宛先を書かない() {
        // `smtp` は空白だけの宛先を飛ばす。`log` も同じに見せる。
        let message = Message {
            to: vec!["  ".into(), "a@example.com".into()],
            cc: vec![String::new()],
            bcc: vec![" ".into(), "b@example.com".into()],
            from: "me@example.com".into(),
            subject: "件名".into(),
            text: Some("本文".into()),
            ..Message::default()
        };
        // 空白だけの写しがあっても、検査は通る（全部の送り先で同じ）。
        assert!(check(&message).is_ok());

        let log = message.to_log();
        assert!(log.contains("To: a@example.com\n"), "{log}");
        assert!(!log.contains("Cc:"), "空の写しは行ごと出さない: {log}");
        assert!(log.contains("Bcc: b@example.com\n"), "{log}");
    }

    /// テストで使う送り先。`install_mailer` を使わずに差し替えられます。
    ///
    /// `ArrayMailer` を使わないのは、溜め先（`SENT`）がプロセス全体で1つで、
    /// 並行して走るほかのテストと取り合うためです。
    struct SpyMailer;

    static SPY: Mutex<Vec<Message>> = Mutex::new(Vec::new());
    static SPY_MAILER: SpyMailer = SpyMailer;

    impl Mailer for SpyMailer {
        fn send(&self, message: &Message) -> Result<()> {
            SPY.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(message.clone());
            Ok(())
        }
    }

    fn spied() -> Vec<Message> {
        std::mem::take(&mut *SPY.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// `SENT` を見るテストを直列にする錠。
    ///
    /// `Mail::clear_sent` を呼ぶテストが並行して走ると、互いの中身を消します。
    static SENT_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn 宛先と本文が無ければ断る() {
        let error = Mail::to("  ")
            .subject("x")
            .text("y")
            .send()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("宛先がありません"));

        let error = Mail::to("a@example.com")
            .subject("x")
            .send()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("本文がありません"));
    }

    #[test]
    fn 差出人の名前は引用符で囲む() {
        assert_eq!(from_header("", "a@example.com"), "a@example.com");
        assert_eq!(from_header("  ", "a@example.com"), "a@example.com");
        assert_eq!(
            from_header("bengara", "a@example.com"),
            "\"bengara\" <a@example.com>"
        );
        // `,` や `"` が入っても壊れない（囲まないと SMTP 側で読めなかった）。
        assert_eq!(
            from_header("Shop, Inc. \"本店\"", "a@example.com"),
            "\"Shop, Inc. \\\"本店\\\"\" <a@example.com>"
        );
        assert_eq!(
            from_header("a\\b", "a@example.com"),
            "\"a\\\\b\" <a@example.com>"
        );
    }

    #[test]
    fn 差出人のアドレスに名前があればアドレスだけを使う() {
        // 以前は `"myapp" <myapp <noreply@example.com>>` と二重に包んでいた。
        assert_eq!(
            from_header("myapp", "myapp <noreply@example.com>"),
            "\"myapp\" <noreply@example.com>"
        );
        assert_eq!(from_header("", " 店 < a@example.com > "), "a@example.com");
        assert_eq!(bare_address("a@example.com"), "a@example.com");
        // `<..>` の形になっていなければ、そのまま。
        assert_eq!(bare_address("a <b@example.com"), "a <b@example.com");
        assert_eq!(bare_address("<>"), "<>");
    }

    #[test]
    fn laravelのenvの名前と値を読める() {
        // MAIL_FROM_ADDRESS（Laravel）が先。空や null なら MAIL_FROM。
        assert_eq!(
            from_address("a@example.com", "b@example.com"),
            "a@example.com"
        );
        assert_eq!(from_address("", "b@example.com"), "b@example.com");
        assert_eq!(from_address("NULL", "b@example.com"), "b@example.com");
        assert_eq!(from_address("", ""), "noreply@example.com");
        // MAIL_USERNAME=null は認証しない（空）。
        assert_eq!(null_as_empty("null".into()), "");
        assert_eq!(null_as_empty(" Null ".into()), "");
        assert_eq!(null_as_empty("nullable".into()), "nullable");
        assert_eq!(null_as_empty("user".into()), "user");
    }

    #[test]
    fn mail_schemeはmail_encryptionより先に見る() {
        // 書かない・null なら MAIL_ENCRYPTION のまま（判断 222 の null も壊さない）。
        assert_eq!(encryption_from("", "tls", 587), "tls");
        assert_eq!(encryption_from("null", "null", 0), "null");
        assert_eq!(encryption_from("NULL", "starttls", 0), "starttls");
        // smtps はポートに関係なく最初から暗号化。
        assert_eq!(encryption_from("smtps", "none", 2525), "smtps");
        // smtp は 465 番なら最初から暗号化、ほかは STARTTLS。
        assert_eq!(encryption_from("smtp", "starttls", 465), "smtps");
        assert_eq!(encryption_from("smtp", "starttls", 587), "starttls");
        assert_eq!(encryption_from(" SMTP ", "tls", 0), "starttls");
        // smtp で MAIL_ENCRYPTION が「暗号化しない」なら、それを残す（Mailpit など）。
        assert_eq!(encryption_from("smtp", "null", 1025), "none");
        assert_eq!(encryption_from("smtp", "none", 1025), "none");
        // 知らない語は MAIL_ENCRYPTION のまま。
        assert_eq!(encryption_from("ftp", "tls", 465), "tls");
    }

    #[test]
    fn 改行の入ったヘッダーは断る() {
        let base = Message {
            to: vec!["a@example.com".to_string()],
            from: "b@example.com".to_string(),
            subject: "件名".to_string(),
            text: Some("本文".into()),
            ..Message::default()
        };
        assert!(check(&base).is_ok());

        // `log` の送り先はヘッダーをそのまま 1 行ずつ書くので、
        // 改行で偽の行を作れた。
        for broken in [
            Message {
                subject: "件名\nBcc: x@example.com".to_string(),
                ..base.clone()
            },
            Message {
                to: vec!["a@example.com\r\nCc: x@example.com".to_string()],
                ..base.clone()
            },
            Message {
                cc: vec!["c@example.com\n".to_string()],
                ..base.clone()
            },
            Message {
                reply_to: Some("r@example.com\r".to_string()),
                ..base.clone()
            },
            Message {
                from: "\"x\ny\" <b@example.com>".to_string(),
                ..base.clone()
            },
        ] {
            let error = check(&broken).unwrap_err().to_string();
            assert!(error.contains("改行は入れられません"), "{error}");
        }
    }

    #[test]
    fn 検査は宛先と本文を見る() {
        // 宛先が1つも無い（空白だけも無いものとして扱う）。
        let message = Message {
            to: vec!["  ".to_string(), String::new()],
            text: Some("本文".into()),
            ..Message::default()
        };
        assert!(check(&message)
            .unwrap_err()
            .to_string()
            .contains("宛先がありません"));

        // 本文が文字でも HTML でも無い。
        let message = Message {
            to: vec!["a@example.com".to_string()],
            ..Message::default()
        };
        assert!(check(&message)
            .unwrap_err()
            .to_string()
            .contains("本文がありません"));

        // HTML だけでも本文はあるものとして通す。
        let message = Message {
            to: vec!["a@example.com".to_string()],
            html: Some("<p>本文</p>".into()),
            ..Message::default()
        };
        assert!(check(&message).is_ok());
    }

    #[tokio::test]
    async fn 送り先を指定して送れる() {
        let _ = spied();
        let message = Mail::to("a@example.com")
            .subject("件名")
            .text("本文")
            .message()
            .clone();
        send_with(&SPY_MAILER, message).await.unwrap();

        let sent = spied();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].subject, "件名");
        assert_eq!(sent[0].to, ["a@example.com"]);

        // 検査に落ちたら送り先には渡さない。
        let message = Message {
            to: vec!["a@example.com".to_string()],
            subject: "本文の無いメール".into(),
            ..Message::default()
        };
        assert!(send_with(&SPY_MAILER, message).await.is_err());
        assert!(spied().is_empty(), "送り先まで届かない");
    }

    #[test]
    fn 送り先は名前で選ぶ() {
        let _guard = SENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let message = Message {
            to: vec!["a@example.com".to_string()],
            subject: "件名".into(),
            text: Some("本文".into()),
            ..Message::default()
        };

        // `array` と `memory` は溜める送り先。大文字と前後の空白は見ない。
        for driver in ["array", "memory", " Array ", "MEMORY"] {
            Mail::clear_sent();
            make_mailer(driver).send(&message).unwrap();
            assert_eq!(Mail::sent().len(), 1, "{driver}");
        }

        // `log` と空は書き出す送り先。溜まりません。
        for driver in ["log", "LOG", " log ", ""] {
            Mail::clear_sent();
            make_mailer(driver).send(&message).unwrap();
            assert!(Mail::sent().is_empty(), "{driver}");
        }

        // 知らない名前は log に落とさず、送るときに断る。
        // 以前は log に書き出し、送れたつもりのまま届かなかった。
        for driver in ["nowhere", "ses"] {
            Mail::clear_sent();
            let error = make_mailer(driver).send(&message).unwrap_err().to_string();
            assert!(
                error.contains(&format!("`{driver}` は知りません")),
                "{error}"
            );
            assert!(error.contains("MAIL_MAILER"), "{error}");
            assert!(Mail::sent().is_empty(), "{driver}");
        }
        Mail::clear_sent();
    }

    #[test]
    fn 送り先の名前はmail_mailerを先に見る() {
        // Laravel の名前。
        assert_eq!(driver_from(Some("smtp"), None), "smtp");
        // 両方あれば `MAIL_MAILER` が勝つ。
        assert_eq!(driver_from(Some("smtp"), Some("log")), "smtp");
        // 空や空白だけなら `MAIL_DRIVER` を見る。
        assert_eq!(driver_from(Some("  "), Some("array")), "array");
        assert_eq!(driver_from(None, Some(" smtp ")), "smtp");
        // どちらも無ければ log。
        assert_eq!(driver_from(None, None), "log");
        assert_eq!(driver_from(Some(""), Some("")), "log");
    }

    #[test]
    fn 空の環境変数は決めていない扱い() {
        // 以前は `MAIL_DRIVER=`（空）でも決めてあると見て、テストが
        // array を使わずに log へ書き出していた。
        assert!(!driver_set(None, None));
        assert!(!driver_set(Some(""), Some("  ")));
        assert!(driver_set(Some("log"), None));
        assert!(driver_set(None, Some("smtp")));
    }

    #[test]
    fn smtp_は_log_にも_array_にも落ちない() {
        let _guard = SENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Mail::clear_sent();
        let mailer = make_mailer("smtp");

        // 機能フラグが無いときは、送ろうとした所で断る。log に落としません。
        #[cfg(not(feature = "mail"))]
        {
            let message = Message {
                to: vec!["a@example.com".to_string()],
                subject: "件名".into(),
                text: Some("本文".into()),
                ..Message::default()
            };
            let error = mailer.send(&message).unwrap_err().to_string();
            assert!(error.contains("features"), "{error}");
            assert!(error.contains("mail"), "{error}");
            // 「`features = ["mail"]` にしてください」だと、そのとおりに書いて
            // `sqlite` を落とす。足すように言う。
            assert!(error.contains("足してください"), "{error}");
            assert!(!error.contains("features = [\"mail\"]"), "{error}");
        }

        // フラグがあるときは、つなぎ口ができる。
        // 実際につなぐのは送るときなので、ここでは送りません
        //（テストがネットワークに出ないようにするため）。
        #[cfg(feature = "mail")]
        let _ = &mailer;

        assert!(Mail::sent().is_empty(), "溜める送り先には渡らない");
        Mail::clear_sent();
    }

    #[tokio::test]
    async fn 溜める送り先は一覧に出る() {
        // install は1回しか効かないので、送り先を直接使う。
        let _guard = SENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Mail::clear_sent();
        let mailer = ArrayMailer;
        let message = Mail::to("a@example.com")
            .subject("件名")
            .text("本文")
            .message()
            .clone();
        mailer.send(&message).unwrap();

        let sent = Mail::sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, ["a@example.com"]);
        assert_eq!(sent[0].subject, "件名");

        Mail::clear_sent();
        assert!(Mail::sent().is_empty());
    }
}
