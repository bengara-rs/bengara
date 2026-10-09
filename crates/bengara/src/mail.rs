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
//! **送り先は3つです。** `MAIL_DRIVER` で選びます。
//!
//! | 送り先 | 何をするか | 要るもの |
//! |---|---|---|
//! | `log`（既定） | `storage/logs/mail.log` に書き出す | 無し |
//! | `array` | プロセスのメモリに溜める（テスト用） | 無し |
//! | `smtp` | SMTP サーバへ送る | Cargo の機能フラグ `mail` |
//!
//! `smtp` は既定では入りません。フラグが無いままでもコンパイルはできて、
//! 送ろうとしたときに直し方を書いたエラーになります。
//!
//! ```toml
//! bengara = { version = "0.1", features = ["mail"] }
//! ```

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
    /// 送り先の名前（`log` / `array` / `smtp`）。
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
    pub encryption: String,
    /// SMTP の利用者名。空のときは認証しません。
    pub username: String,
    /// SMTP のパスワード。
    pub password: String,
    /// `EHLO` で名乗る名前。空のときは lettre の既定（`localhost`）です。
    pub ehlo_name: String,
    /// 返事を待つ上限（秒）。
    pub timeout: u64,
}

impl Default for MailConfig {
    /// 環境変数から組み立てます。
    ///
    /// | 環境変数          | 既定値                 |
    /// |-------------------|------------------------|
    /// | `MAIL_DRIVER`     | `log`                  |
    /// | `MAIL_FROM`       | `noreply@example.com`  |
    /// | `MAIL_FROM_NAME`  | `bengara`              |
    /// | `MAIL_HOST`       | `127.0.0.1`            |
    /// | `MAIL_PORT`       | 0（暗号化から決める）  |
    /// | `MAIL_ENCRYPTION` | `starttls`             |
    /// | `MAIL_USERNAME`   | 空（認証しない）       |
    /// | `MAIL_PASSWORD`   | 空                     |
    /// | `MAIL_EHLO_NAME`  | 空                     |
    /// | `MAIL_TIMEOUT`    | 10（秒）               |
    fn default() -> Self {
        Self {
            driver: crate::env("MAIL_DRIVER", "log"),
            from: crate::env("MAIL_FROM", "noreply@example.com"),
            from_name: crate::env("MAIL_FROM_NAME", "bengara"),
            host: crate::env("MAIL_HOST", "127.0.0.1"),
            port: crate::env::<u16>("MAIL_PORT", 0u16),
            encryption: crate::env("MAIL_ENCRYPTION", "starttls"),
            username: crate::env("MAIL_USERNAME", ""),
            password: crate::env("MAIL_PASSWORD", ""),
            ehlo_name: crate::env("MAIL_EHLO_NAME", ""),
            timeout: crate::env::<u64>("MAIL_TIMEOUT", 10u64),
        }
    }
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
        out.push_str(&format!("To: {}\n", self.to.join(", ")));
        if !self.cc.is_empty() {
            out.push_str(&format!("Cc: {}\n", self.cc.join(", ")));
        }
        if !self.bcc.is_empty() {
            out.push_str(&format!("Bcc: {}\n", self.bcc.join(", ")));
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
pub(crate) fn install_test_mailer() {
    if std::env::var("MAIL_DRIVER").is_ok() {
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

/// 名前から送り先を作る。**知らない名前は `log`** にします。
///
/// `mailer()` から切り出してあります。`MAILER` は 1 回しか決まらないので、
/// 中に書くと選び方をテストから確かめられません。
fn make_mailer(driver: &str) -> Box<dyn Mailer> {
    match driver {
        "array" | "memory" => Box::new(ArrayMailer) as Box<dyn Mailer>,
        "log" => Box::new(LogMailer),
        "smtp" => make_smtp_mailer(),
        other => {
            tracing::warn!("MAIL_DRIVER `{other}` は知りません。log を使います");
            Box::new(LogMailer)
        }
    }
}

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
            reason: "SMTP で送るには Cargo.toml を \
                     `bengara = { version = \"0.1\", features = [\"mail\"] }` にしてください"
                .to_string(),
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
                from: if config.from_name.is_empty() {
                    config.from.clone()
                } else {
                    format!("{} <{}>", config.from_name, config.from)
                },
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

/// 送れる形になっているか。
///
/// 宛先が無いメールと本文が無いメールは、送り先に渡す前に断ります。
/// 送り先によっては黙って捨てられ、送れたつもりになるためです。
fn check(message: &Message) -> Result<()> {
    if message.to.iter().all(|a| a.trim().is_empty()) {
        return Err(Error::msg("メールの宛先がありません"));
    }
    if message.text.is_none() && message.html.is_none() {
        return Err(Error::msg("メールの本文がありません"));
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

        // `array` と `memory` は溜める送り先。
        for driver in ["array", "memory"] {
            Mail::clear_sent();
            make_mailer(driver).send(&message).unwrap();
            assert_eq!(Mail::sent().len(), 1, "{driver}");
        }

        // `log` と知らない名前は書き出す送り先。溜まりません。
        for driver in ["log", "nowhere", ""] {
            Mail::clear_sent();
            make_mailer(driver).send(&message).unwrap();
            assert!(Mail::sent().is_empty(), "{driver}");
        }
        Mail::clear_sent();
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
