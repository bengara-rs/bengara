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
//! **送り先は2つだけです。**
//!
//! | 送り先 | 何をするか |
//! |---|---|
//! | `log`（既定） | `storage/logs/mail.log` に書き出す |
//! | `array` | プロセスのメモリに溜める（テスト用） |
//!
//! **SMTP はまだありません。** TLS を自前で書けないためです。
//! 形（`Mailer`）は用意してあるので、決まれば差し替えられます。

use std::sync::{Mutex, OnceLock};

use crate::error::{Error, Result};

/// メールの設定。`config/mail.rs` が返します。
#[derive(Debug, Clone)]
pub struct MailConfig {
    /// 送り先の名前（`log` / `array`）。
    pub driver: String,
    /// 差出人（`.from(..)` を書かなかったときに使う）。
    pub from: String,
    /// 差出人の名前。
    pub from_name: String,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            driver: crate::env("MAIL_DRIVER", "log"),
            from: crate::env("MAIL_FROM", "noreply@example.com"),
            from_name: crate::env("MAIL_FROM_NAME", "bengara"),
        }
    }
}

fn config() -> MailConfig {
    crate::try_config::<MailConfig>()
        .cloned()
        .unwrap_or_default()
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
pub fn install_mailer(mailer: Box<dyn Mailer>) {
    let _ = MAILER.set(mailer);
}

/// テストのときに `array` を使う。`#[bengara::test]` が呼びます。
pub(crate) fn install_test_mailer() {
    if std::env::var("MAIL_DRIVER").is_ok() {
        return;
    }
    install_mailer(Box::new(ArrayMailer));
}

fn mailer() -> &'static dyn Mailer {
    MAILER
        .get_or_init(|| {
            let config = config();
            match config.driver.as_str() {
                "array" | "memory" => Box::new(ArrayMailer) as Box<dyn Mailer>,
                "log" => Box::new(LogMailer),
                other => {
                    tracing::warn!("MAIL_DRIVER `{other}` は知りません。log を使います");
                    Box::new(LogMailer)
                }
            }
        })
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
                    config.from
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
        let message = self.message;
        if message.to.iter().all(|a| a.trim().is_empty()) {
            return Err(Error::msg("メールの宛先がありません"));
        }
        if message.text.is_none() && message.html.is_none() {
            return Err(Error::msg("メールの本文がありません"));
        }
        crate::support::blocking(move || mailer().send(&message)).await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 既定の設定() {
        let config = MailConfig::default();
        assert_eq!(config.driver, "log");
        assert!(config.from.contains('@'));
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

    #[tokio::test]
    async fn 溜める送り先は一覧に出る() {
        // install は1回しか効かないので、送り先を直接使う。
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
