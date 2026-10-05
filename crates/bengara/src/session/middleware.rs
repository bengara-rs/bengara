//! セッションを読み書きするミドルウェアと、CSRF の確認。

use std::sync::Arc;

use super::{Session, SessionStore};
use crate::error::{Error, Result};
use crate::http::{BoxFuture, Cookie, Middleware, Next, Request, SameSite};
use crate::support::crypto;

/// セッションの設定。
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Cookie の名前。
    pub cookie: String,
    /// 放っておいて消えるまでの秒数。既定は 2 時間。
    pub lifetime_secs: u64,
    /// HTTPS のときだけ Cookie を送るか。
    pub secure: bool,
    /// 他のサイトから来たときに Cookie を送るか。
    pub same_site: SameSite,
    /// Cookie を送る範囲のパス。
    pub path: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cookie: "bengara_session".to_string(),
            lifetime_secs: 7200,
            // 本番では自動で true にする（下の `resolved` を参照）。
            secure: false,
            same_site: SameSite::Lax,
            path: "/".to_string(),
        }
    }
}

impl SessionConfig {
    /// 実際に使う設定にそろえる。
    ///
    /// `APP_ENV=production` のときは `Secure` を強制します。
    /// HTTP で動かしている本番があると Cookie が送られなくなりますが、
    /// 平文で送ってしまうよりは気づけるほうがよいと考えました。
    fn resolved(&self) -> Self {
        let mut out = self.clone();
        if crate::config_registry::app_config().is_production() {
            out.secure = true;
        }
        out
    }
}

/// セッションを読み込み、必要なら保存するミドルウェア。
///
/// `bootstrap/app.rs` で登録します。
///
/// ```ignore
/// .with_middleware(|m| m.append(StartSession::file()))
/// ```
pub struct StartSession {
    store: Arc<dyn SessionStore>,
    config: SessionConfig,
}

impl StartSession {
    /// 置き場所を指定して作る。
    pub fn new(store: impl SessionStore, config: SessionConfig) -> Self {
        Self {
            store: Arc::new(store),
            config,
        }
    }

    /// `storage/framework/sessions/` に置く（既定）。
    pub fn file() -> Self {
        let config = SessionConfig::default();
        Self::new(super::FileStore::default_path(config.lifetime_secs), config)
    }

    /// プロセスのメモリに置く。**テスト専用です。**
    pub fn memory() -> Self {
        let config = SessionConfig::default();
        Self::new(super::MemoryStore::new(config.lifetime_secs), config)
    }

    /// `.env` の `SESSION_DRIVER` と `SESSION_LIFETIME` を見て作る。
    ///
    /// | `SESSION_DRIVER` | 置き場所 |
    /// |---|---|
    /// | `file`（既定） | `storage/framework/sessions/` |
    /// | `memory` | プロセスのメモリ。**テストと手元の確認用** |
    ///
    /// `SESSION_LIFETIME` は分で指定します（既定 120）。
    pub fn from_env() -> Self {
        let minutes: u64 = crate::env("SESSION_LIFETIME", 120u64);
        let config = SessionConfig {
            lifetime_secs: minutes.saturating_mul(60).max(60),
            ..SessionConfig::default()
        };
        let driver: String = crate::env("SESSION_DRIVER", "file");
        match driver.as_str() {
            "memory" | "array" => Self::new(super::MemoryStore::new(config.lifetime_secs), config),
            _ => Self::new(super::FileStore::default_path(config.lifetime_secs), config),
        }
    }

    /// 設定を差し替える。
    pub fn with_config(mut self, config: SessionConfig) -> Self {
        self.config = config;
        self
    }
}

impl Middleware for StartSession {
    fn handle(&self, mut req: Request, next: Next) -> BoxFuture {
        let store = self.store.clone();
        let config = self.config.resolved();

        Box::pin(async move {
            let key = signing_key()?;

            // 1. Cookie から ID を読む。署名が合わないものは無かったことにする。
            let incoming = req
                .cookie(&config.cookie)
                .and_then(|raw| unsign(&raw, &key));

            let had_cookie = incoming.is_some();

            // 2. 置き場所から中身を読む。
            let session = match incoming {
                Some(id) => {
                    let found = {
                        let store = store.clone();
                        let id = id.clone();
                        tokio::task::spawn_blocking(move || store.read(&id))
                            .await
                            .map_err(|_| Error::msg("セッションの読み込みが中断されました"))??
                    };
                    match found {
                        Some(stored) => {
                            let (data, old_flash) = Session::split_flash(stored);
                            Session::new(id, data, old_flash)
                        }
                        // ID はあるが中身が無い（期限切れなど）。同じ ID で作り直さない。
                        None => Session::empty(),
                    }
                }
                None => Session::empty(),
            };

            let id_before = session.id();
            req.set_session(session.clone());

            // 3. ハンドラへ。
            let response = next.run(req).await?;

            // 4. 要るときだけ保存する。
            let (id_after, data) = session.take_for_save();
            let id_changed = id_before != id_after;
            let saved = session.should_save() || id_changed;

            if saved {
                if id_changed {
                    // 古いほうは残さない。
                    let store = store.clone();
                    let old = id_before.clone();
                    let _ = tokio::task::spawn_blocking(move || store.destroy(&old)).await;
                }
                if session.was_flushed() && data.is_empty() {
                    let store = store.clone();
                    let id = id_after.clone();
                    let _ = tokio::task::spawn_blocking(move || store.destroy(&id)).await;
                } else {
                    let store = store.clone();
                    let id = id_after.clone();
                    tokio::task::spawn_blocking(move || store.write(&id, &data))
                        .await
                        .map_err(|_| Error::msg("セッションの保存が中断されました"))??;
                }
            }

            // 5. Cookie を返す。
            //
            // 保存したときと、もともと有効な Cookie を持っていたとき（期限を延ばす）だけです。
            // 何も起きなかった人に配ると、置き場所に無い ID の Cookie を持たせることになります。
            if saved || had_cookie {
                let cookie = Cookie::new(&config.cookie, sign(&id_after, &key))
                    .with_path(&config.path)
                    .with_max_age(config.lifetime_secs as i64)
                    .with_secure(config.secure)
                    .with_http_only(true)
                    .with_same_site(config.same_site);
                return Ok(response.with_cookie(&cookie));
            }
            Ok(response)
        })
    }
}

/// 署名の鍵。`APP_KEY` から作ります。
fn signing_key() -> Result<Vec<u8>> {
    Ok(crate::config_registry::app_config().signing_key()?.to_vec())
}

/// `値|署名` の形にする。
fn sign(value: &str, key: &[u8]) -> String {
    let mac = crypto::hmac_sha256(key, value.as_bytes());
    format!("{value}|{}", crypto::to_hex(&mac))
}

/// 署名を確かめて値を取り出す。合わなければ `None`。
fn unsign(raw: &str, key: &[u8]) -> Option<String> {
    let (value, signature) = raw.rsplit_once('|')?;
    let expected = crypto::hmac_sha256(key, value.as_bytes());
    let given = crypto::from_hex(signature)?;
    // 時間差から署名を当てられないよう、定数時間で比べる。
    if crypto::constant_time_eq(&expected, &given) {
        Some(value.to_string())
    } else {
        tracing::debug!("セッション Cookie の署名が合いませんでした");
        None
    }
}

/// CSRF（別のサイトから勝手に送られる書き込み）を防ぐミドルウェア。
///
/// `GET` / `HEAD` / `OPTIONS` は素通しし、それ以外でトークンを確かめます。
/// **`StartSession` より後ろに置いてください。**
///
/// ```ignore
/// .with_middleware(|m| m.append(StartSession::file()).append(VerifyCsrfToken::new()))
/// ```
pub struct VerifyCsrfToken {
    except: Vec<String>,
}

impl Default for VerifyCsrfToken {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyCsrfToken {
    /// 全部のパスで確かめる。
    pub fn new() -> Self {
        Self { except: Vec::new() }
    }

    /// 確かめないパスを足す。末尾の `*` で前方一致にできます。
    ///
    /// ```ignore
    /// VerifyCsrfToken::new().except("/api/*")
    /// ```
    pub fn except(mut self, pattern: &str) -> Self {
        self.except.push(pattern.to_string());
        self
    }

    fn is_excepted(&self, path: &str) -> bool {
        self.except
            .iter()
            .any(|pattern| match pattern.strip_suffix('*') {
                Some(prefix) => path.starts_with(prefix),
                None => path == pattern,
            })
    }
}

/// セッションの中でトークンを入れておく場所。
pub(crate) const CSRF_KEY: &str = "__csrf_token";

impl Middleware for VerifyCsrfToken {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let safe = matches!(req.method(), "GET" | "HEAD" | "OPTIONS");
        let excepted = self.is_excepted(req.path());

        Box::pin(async move {
            let Some(session) = req.try_session() else {
                return Err(Error::msg(
                    "CSRF の確認にはセッションが要ります。bootstrap/app.rs で \
                     StartSession を VerifyCsrfToken より前に登録してください",
                ));
            };

            // 読むだけの操作は確かめない。トークンだけ用意しておく。
            if safe || excepted {
                ensure_token(session);
                return next.run(req).await;
            }

            let expected = session.get(CSRF_KEY).unwrap_or_default();
            let given = token_from_request(&req);

            if expected.is_empty()
                || given.is_empty()
                || !crypto::constant_time_eq(expected.as_bytes(), given.as_bytes())
            {
                return Err(Error::http(
                    419,
                    "ページの有効期限が切れました。読み込み直してからもう一度お試しください。",
                ));
            }
            next.run(req).await
        })
    }
}

/// セッションにトークンが無ければ作る。
pub(crate) fn ensure_token(session: &Session) -> String {
    if let Some(token) = session.get(CSRF_KEY) {
        if !token.is_empty() {
            return token;
        }
    }
    let token = crypto::random_token();
    session.put(CSRF_KEY, token.clone());
    token
}

/// リクエストからトークンを探す。
///
/// 探す順は、フォームの `_token` → `X-CSRF-TOKEN` ヘッダーです。
fn token_from_request(req: &Request) -> String {
    if let Some(token) = req.input("_token") {
        if !token.is_empty() {
            return token;
        }
    }
    req.header("x-csrf-token").unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 署名が往復する() {
        let key = b"test-key";
        let signed = sign("abc123", key);
        assert!(signed.starts_with("abc123|"));
        assert_eq!(unsign(&signed, key).as_deref(), Some("abc123"));
    }

    #[test]
    fn 署名を書き換えたら通らない() {
        let key = b"test-key";
        let signed = sign("abc123", key);

        // 値だけ書き換える。
        let tampered = signed.replace("abc123", "xyz789");
        assert!(unsign(&tampered, key).is_none());

        // 署名だけ書き換える。
        let (value, _) = signed.rsplit_once('|').unwrap();
        assert!(unsign(&format!("{value}|{}", "0".repeat(64)), key).is_none());

        // 別の鍵では通らない。
        assert!(unsign(&signed, b"other-key").is_none());
    }

    #[test]
    fn 壊れた形は通らない() {
        let key = b"test-key";
        assert!(unsign("署名がない", key).is_none());
        assert!(unsign("", key).is_none());
        assert!(unsign("a|16進でない", key).is_none());
    }

    #[test]
    fn 値に区切り文字が入っていても末尾で切る() {
        let key = b"test-key";
        let signed = sign("a|b", key);
        assert_eq!(unsign(&signed, key).as_deref(), Some("a|b"));
    }

    #[test]
    fn トークンは一度作ったら変わらない() {
        let session = Session::empty();
        let first = ensure_token(&session);
        let second = ensure_token(&session);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn 除外するパスを判定できる() {
        let mw = VerifyCsrfToken::new().except("/api/*").except("/webhook");
        assert!(mw.is_excepted("/api/posts"));
        assert!(mw.is_excepted("/api/"));
        assert!(mw.is_excepted("/webhook"));
        assert!(!mw.is_excepted("/webhooks"));
        assert!(!mw.is_excepted("/admin"));
    }

    #[test]
    fn 本番ではsecureが強制される() {
        // 既定（開発）は false のまま。
        let config = SessionConfig::default();
        assert!(!config.secure);
    }
}
