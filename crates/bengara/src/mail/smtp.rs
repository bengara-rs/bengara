//! SMTP でメールを送る（lettre）。
//!
//! **lettre を知っているのはこのファイルだけです。** 上の層は [`Message`] しか
//! 扱いません。Cargo の機能フラグ `mail` を付けたときだけコンパイルされます。
//!
//! 待つ形（同期）の送り口を使います。[`Mailer::send`] は
//! `spawn_blocking` の中で呼ばれるので、別のスレッドで待っても
//! リクエストの処理は止まりません。

use std::time::Duration;

use lettre::message::{MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{Message as LettreMessage, SmtpTransport, Transport};

use super::{MailConfig, Mailer, Message};
use crate::error::{Error, Result};

/// 暗号化の仕方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encryption {
    /// つないだ直後から暗号化する（ふつうは 465 番）。
    Tls,
    /// 平文でつないで `STARTTLS` で切り替える（ふつうは 587 番）。
    StartTls,
    /// 暗号化しない（ふつうは 25 番）。**社内の中継だけで使ってください。**
    None,
}

impl Encryption {
    /// 設定の文字列から読む。
    ///
    /// Laravel の `MAIL_ENCRYPTION` に合わせて `tls` / `ssl` / `starttls` /
    /// `null` を受け付けます。知らない語は `starttls` にします。
    pub(super) fn parse(name: &str) -> Encryption {
        match name.trim().to_ascii_lowercase().as_str() {
            "tls" | "ssl" | "smtps" => Encryption::Tls,
            "none" | "null" | "" | "false" => Encryption::None,
            "starttls" => Encryption::StartTls,
            other => {
                tracing::warn!("MAIL_ENCRYPTION `{other}` は知りません。starttls を使います");
                Encryption::StartTls
            }
        }
    }

    /// ポートを書かなかったときに使う番号。
    pub(super) fn default_port(self) -> u16 {
        match self {
            Encryption::Tls => 465,
            Encryption::StartTls => 587,
            Encryption::None => 25,
        }
    }
}

/// SMTP で送る送り先。
pub(super) struct SmtpMailer {
    transport: SmtpTransport,
}

impl SmtpMailer {
    /// 設定からつなぎ口を組み立てる。**まだつなぎません。**
    ///
    /// 実際につなぐのは1通目を送るときです。`lettre` が接続を溜めておくので、
    /// 2通目からは TLS の握り直しが起きません。
    pub(super) fn new(config: &MailConfig) -> Result<Self> {
        let encryption = Encryption::parse(&config.encryption);
        if config.host.trim().is_empty() {
            return Err(Error::msg(
                "MAIL_HOST が空です。SMTP サーバの名前を書いてください",
            ));
        }

        let mut builder = match encryption {
            Encryption::Tls => SmtpTransport::relay(&config.host),
            Encryption::StartTls => SmtpTransport::starttls_relay(&config.host),
            // 暗号化なしは `Result` を返しません。形をそろえるため包み直します。
            Encryption::None => Ok(SmtpTransport::builder_dangerous(&config.host)),
        }
        .map_err(|e| {
            Error::msg(format!(
                "SMTP の設定を組み立てられません（{}）: {e}",
                config.host
            ))
        })?;

        let port = if config.port == 0 {
            encryption.default_port()
        } else {
            config.port
        };
        builder = builder.port(port).timeout(Some(Duration::from_secs(
            // 0 を渡すとすぐ諦めるので、下限を 1 秒にする。
            config.timeout.max(1),
        )));

        if !config.ehlo_name.trim().is_empty() {
            builder = builder.hello_name(ClientId::Domain(config.ehlo_name.trim().to_string()));
        }

        if !config.username.is_empty() {
            if encryption == Encryption::None {
                // 平文のまま利用者名とパスワードが流れます。黙って通しません。
                tracing::warn!(
                    "MAIL_ENCRYPTION が none のまま MAIL_USERNAME を使います。\
                     利用者名とパスワードが暗号化されずに流れます"
                );
            }
            builder = builder.credentials(Credentials::new(
                config.username.clone(),
                config.password.clone(),
            ));
        }

        Ok(Self {
            transport: builder.build(),
        })
    }
}

impl Mailer for SmtpMailer {
    fn send(&self, message: &Message) -> Result<()> {
        let built = build(message)?;
        self.transport
            .send(&built)
            .map_err(|e| Error::msg(format!("メールを送れません: {e}")))?;
        Ok(())
    }
}

/// bengara の [`Message`] を lettre の文面に直す。
///
/// 宛先は `alice@example.com` と `アリス <alice@example.com>` の両方を
/// 受け付けます。読めないときは、どの欄のどの文字列かを添えて断ります。
fn build(message: &Message) -> Result<LettreMessage> {
    let mut builder = LettreMessage::builder().from(mailbox("From", &message.from)?);
    for address in &message.to {
        if address.trim().is_empty() {
            continue;
        }
        builder = builder.to(mailbox("To", address)?);
    }
    for address in &message.cc {
        builder = builder.cc(mailbox("Cc", address)?);
    }
    for address in &message.bcc {
        builder = builder.bcc(mailbox("Bcc", address)?);
    }
    if let Some(reply_to) = &message.reply_to {
        builder = builder.reply_to(mailbox("Reply-To", reply_to)?);
    }
    builder = builder.subject(message.subject.clone());

    let built = match (&message.text, &message.html) {
        (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
            text.clone(),
            html.clone(),
        )),
        (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone())),
        (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone())),
        // 本文が無いメールは `mail.rs` の `check` が先に断ります。
        (None, None) => return Err(Error::msg("メールの本文がありません")),
    };
    built.map_err(|e| Error::msg(format!("メールの文面を組み立てられません: {e}")))
}

/// 1つのアドレスを読む。
fn mailbox(field: &str, address: &str) -> Result<lettre::message::Mailbox> {
    address
        .trim()
        .parse()
        .map_err(|e| Error::msg(format!("{field} のアドレス `{address}` が読めません: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 暗号化の名前を読む() {
        assert_eq!(Encryption::parse("tls"), Encryption::Tls);
        assert_eq!(Encryption::parse("SSL"), Encryption::Tls);
        assert_eq!(Encryption::parse("starttls"), Encryption::StartTls);
        assert_eq!(Encryption::parse("none"), Encryption::None);
        assert_eq!(Encryption::parse("null"), Encryption::None);
        assert_eq!(Encryption::parse(""), Encryption::None);
        // 知らない語は、暗号化する側に倒す。
        assert_eq!(Encryption::parse("なにか"), Encryption::StartTls);
    }

    #[test]
    fn 既定のポートは暗号化で変わる() {
        assert_eq!(Encryption::Tls.default_port(), 465);
        assert_eq!(Encryption::StartTls.default_port(), 587);
        assert_eq!(Encryption::None.default_port(), 25);
    }

    fn config() -> MailConfig {
        MailConfig {
            driver: "smtp".into(),
            host: "smtp.example.com".into(),
            ..MailConfig::default()
        }
    }

    #[test]
    fn ホストが空なら断る() {
        let config = MailConfig {
            host: "  ".into(),
            ..config()
        };
        // `SmtpMailer` は Debug を持たないので、`unwrap_err` は使えません。
        let error = match SmtpMailer::new(&config) {
            Ok(_) => panic!("ホストが空なら断るはず"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("MAIL_HOST"), "{error}");
    }

    #[test]
    fn 設定からつなぎ口を組み立てられる() {
        // つなぎません。組み立てだけを確かめます。
        for encryption in ["tls", "starttls", "none"] {
            let config = MailConfig {
                encryption: encryption.into(),
                ..config()
            };
            assert!(SmtpMailer::new(&config).is_ok(), "{encryption}");
        }
    }

    fn message() -> Message {
        Message {
            to: vec!["alice@example.com".into()],
            from: "bengara <noreply@example.com>".into(),
            subject: "件名".into(),
            text: Some("本文".into()),
            ..Message::default()
        }
    }

    #[test]
    fn 名前付きのアドレスも読める() {
        let built = build(&message()).expect("組み立てられる");
        let raw = String::from_utf8(built.formatted()).expect("UTF-8 で出る");
        assert!(raw.contains("alice@example.com"), "{raw}");
        assert!(raw.contains("noreply@example.com"), "{raw}");
    }

    #[test]
    fn 読めないアドレスは欄の名前を添えて断る() {
        let message = Message {
            to: vec!["これはアドレスではない".into()],
            ..message()
        };
        let error = build(&message).unwrap_err().to_string();
        assert!(error.contains("To"), "{error}");
        assert!(error.contains("これはアドレスではない"), "{error}");
    }

    #[test]
    fn 空白だけの宛先は飛ばす() {
        // `check` が宛先ゼロを断るので、ここに来るのは1つ以上あるときだけ。
        let message = Message {
            to: vec!["  ".into(), "alice@example.com".into()],
            ..message()
        };
        let raw = String::from_utf8(build(&message).unwrap().formatted()).unwrap();
        assert!(raw.contains("alice@example.com"), "{raw}");
    }

    #[test]
    fn 本文の組み合わせで形が変わる() {
        // 文字だけ。
        let raw = String::from_utf8(build(&message()).unwrap().formatted()).unwrap();
        assert!(raw.contains("text/plain"), "{raw}");
        assert!(!raw.contains("multipart/alternative"), "{raw}");

        // HTML だけ。
        let html_only = Message {
            text: None,
            html: Some("<p>本文</p>".into()),
            ..message()
        };
        let raw = String::from_utf8(build(&html_only).unwrap().formatted()).unwrap();
        assert!(raw.contains("text/html"), "{raw}");

        // 両方。読む側が選べるように multipart/alternative にする。
        let both = Message {
            html: Some("<p>本文</p>".into()),
            ..message()
        };
        let raw = String::from_utf8(build(&both).unwrap().formatted()).unwrap();
        assert!(raw.contains("multipart/alternative"), "{raw}");
    }
    /// 本物の SMTP サーバにつないで送る。
    ///
    /// **サーバが無いときは何もしません。** CI に SMTP サーバを置いていないためです。
    /// 手元で確かめるときは、例えば次のように書きます。
    ///
    /// ```sh
    /// BENGARA_TEST_SMTP_HOST=127.0.0.1 BENGARA_TEST_SMTP_PORT=11025 \
    ///   cargo test -p bengara --features mail
    /// ```
    fn live_config() -> Option<MailConfig> {
        let host = std::env::var("BENGARA_TEST_SMTP_HOST")
            .ok()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())?;
        let port: u16 = std::env::var("BENGARA_TEST_SMTP_PORT")
            .ok()
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(25);
        Some(MailConfig {
            driver: "smtp".into(),
            host,
            port,
            // 手元の確認用サーバは暗号化していないことが多い。
            encryption: "none".into(),
            ..MailConfig::default()
        })
    }

    #[test]
    fn 本物のサーバに送れる() {
        let Some(config) = live_config() else { return };
        let mailer = match SmtpMailer::new(&config) {
            Ok(mailer) => mailer,
            Err(error) => panic!("つなぎ口を組み立てられない: {error}"),
        };

        let message = Message {
            to: vec!["alice@example.com".into()],
            cc: vec!["bob@example.com".into()],
            bcc: vec!["carol@example.com".into()],
            from: "bengara <noreply@example.com>".into(),
            reply_to: Some("reply@example.com".into()),
            subject: "件名の確認".into(),
            text: Some("本文の確認".into()),
            html: Some("<p>本文の確認</p>".into()),
        };
        mailer.send(&message).expect("送れる");

        // 読めないアドレスは、つなぐ前に断る。
        let broken = Message {
            to: vec!["これはアドレスではない".into()],
            ..message
        };
        assert!(mailer.send(&broken).is_err());
    }
}
