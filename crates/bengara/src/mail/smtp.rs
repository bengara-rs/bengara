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
    ///
    /// **空の値は「書かなかった」と同じ `starttls` です。** `.env` の空の値は
    /// `env()` が既定値（`starttls`）に読み替えます。`config/mail.rs` で空の文字列を
    /// 渡したときだけ暗号化なしになると、同じ「空」で意味が食い違うためです。
    /// 暗号化なしにしたいときは `none` と書きます。
    pub(super) fn parse(name: &str) -> Encryption {
        match name.trim().to_ascii_lowercase().as_str() {
            "tls" | "ssl" | "smtps" => Encryption::Tls,
            "none" | "null" | "false" => Encryption::None,
            "starttls" | "" => Encryption::StartTls,
            other => {
                tracing::warn!("MAIL_ENCRYPTION `{other}` は知りません。starttls を使います");
                Encryption::StartTls
            }
        }
    }

    /// 設定の文字列とポートから、使う暗号化とポートを決める。
    ///
    /// `tls` と `ssl` は、ポートが 465 か 0（書かなかった）のときだけ
    /// つないだ直後から暗号化します。**ほかのポートでは `STARTTLS`** にします。
    /// Laravel（Symfony Mailer）と同じ決め方です。Laravel の `.env` は
    /// `MAIL_ENCRYPTION=tls` と `MAIL_PORT=587` の組み合わせがふつうで、
    /// 以前はこれを 587 番へいきなり TLS でつなぎ、握手に失敗していました。
    ///
    /// `smtps` はポートに関係なく、つないだ直後から暗号化します
    /// （名前そのものが「最初から TLS」の意味なので）。
    pub(super) fn resolve(name: &str, port: u16) -> (Encryption, u16) {
        let parsed = Encryption::parse(name);
        let implicit_only = name.trim().eq_ignore_ascii_case("smtps");
        let encryption = match parsed {
            Encryption::Tls if !implicit_only && port != 0 && port != 465 => Encryption::StartTls,
            other => other,
        };
        let port = if port == 0 {
            encryption.default_port()
        } else {
            port
        };
        (encryption, port)
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
        let (encryption, port) = Encryption::resolve(&config.encryption, config.port);
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
    // 写しも宛先と同じく、空白だけのものは飛ばす。`log` と `array` は
    // 空の写しをそのまま通すので、ここだけ断ると SMTP に切り替えて初めて落ちます。
    for address in &message.cc {
        if address.trim().is_empty() {
            continue;
        }
        builder = builder.cc(mailbox("Cc", address)?);
    }
    for address in &message.bcc {
        if address.trim().is_empty() {
            continue;
        }
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
        // 空の値は「書かなかった」と同じ。`.env` の空の値（`env()` が既定値の
        // starttls に読み替える）と同じ意味にそろえる。
        assert_eq!(Encryption::parse(""), Encryption::StartTls);
        assert_eq!(Encryption::parse("  "), Encryption::StartTls);
        assert_eq!(Encryption::resolve("", 0), (Encryption::StartTls, 587));
        // 知らない語は、暗号化する側に倒す。
        assert_eq!(Encryption::parse("なにか"), Encryption::StartTls);
    }

    #[test]
    fn tlsは465番以外ではstarttlsにする() {
        // Laravel の `.env` によくある組み合わせ。以前は 587 番へいきなり
        // TLS でつないで、握手に失敗していた。
        assert_eq!(Encryption::resolve("tls", 587), (Encryption::StartTls, 587));
        assert_eq!(
            Encryption::resolve("ssl", 2525),
            (Encryption::StartTls, 2525)
        );
        // 465 番と、ポートを書かないときは最初から TLS。
        assert_eq!(Encryption::resolve("tls", 465), (Encryption::Tls, 465));
        assert_eq!(Encryption::resolve("TLS", 0), (Encryption::Tls, 465));
        // `smtps` はポートに関係なく最初から TLS。
        assert_eq!(Encryption::resolve("smtps", 2465), (Encryption::Tls, 2465));
        // ほかの語はポートで変えない。
        assert_eq!(
            Encryption::resolve("starttls", 0),
            (Encryption::StartTls, 587)
        );
        assert_eq!(Encryption::resolve("none", 0), (Encryption::None, 25));
        assert_eq!(Encryption::resolve("none", 1025), (Encryption::None, 1025));
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
    fn 空白だけの写しも飛ばす() {
        // 以前は「Cc のアドレスが読めません」で落ちた。`log` / `array` では
        // 通るので、SMTP に切り替えて初めて分かった。
        let message = Message {
            cc: vec![String::new(), "  ".into(), "bob@example.com".into()],
            bcc: vec![" ".into()],
            ..message()
        };
        let built = build(&message).expect("空白だけの写しでは落ちない");
        let raw = String::from_utf8(built.formatted()).unwrap();
        assert!(raw.contains("bob@example.com"), "{raw}");
        // 送り先（封筒）は宛先と、空でない写しの 2 つだけ。
        assert_eq!(built.envelope().to().len(), 2);

        // 読めない写しは、今までどおり欄の名前を添えて断る。
        let broken = Message {
            cc: vec!["これはアドレスではない".into()],
            ..self::message()
        };
        let error = build(&broken).unwrap_err().to_string();
        assert!(error.contains("Cc"), "{error}");
    }

    #[test]
    fn 既定の差出人の形を組み立てに通せる() {
        // `Mail::to` は差出人を `"名前" <アドレス>`（引用符つき）で作る。
        // この形を lettre が読めることを押さえる。
        for name in ["bengara", "べんがら商店", "Shop, Inc. \"本店\"", "a\\b"] {
            let from = super::super::from_header(name, "noreply@example.com");
            assert!(from.starts_with('"'), "引用符つきで作る: {from}");
            let message = Message { from, ..message() };
            let built = build(&message).unwrap_or_else(|e| panic!("{name}: {e}"));
            // 封筒の差出人はアドレスだけになる。
            assert_eq!(
                built.envelope().from().map(|a| a.to_string()).as_deref(),
                Some("noreply@example.com"),
                "{name}"
            );
            let raw = String::from_utf8(built.formatted()).unwrap();
            assert!(raw.contains("<noreply@example.com>"), "{name}: {raw}");
        }

        // 日本語の名前は符号化して出る（ヘッダーに生の日本語を書かない）。
        let message = Message {
            from: super::super::from_header("べんがら商店", "noreply@example.com"),
            ..message()
        };
        let raw = String::from_utf8(build(&message).unwrap().formatted()).unwrap();
        let from_line = raw
            .lines()
            .find(|line| line.starts_with("From:"))
            .expect("From の行がある");
        assert!(from_line.is_ascii(), "{from_line}");
        assert!(from_line.contains("=?utf-8?b?"), "{from_line}");

        // 名前が空なら、アドレスだけで通る。
        let message = Message {
            from: super::super::from_header("", "noreply@example.com"),
            ..self::message()
        };
        assert!(build(&message).is_ok());
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
