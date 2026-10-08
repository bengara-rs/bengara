//! セッションを読み書きするミドルウェアと、CSRF の確認。

use std::sync::{Arc, OnceLock};

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
    ///
    /// **組み立てるときに 1 回だけ呼びます。** `APP_ENV` は起動後に変わりません。
    /// リクエストごとに呼ぶと、毎回クローンが走ります。
    fn resolved(self) -> Self {
        let mut out = self;
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
    ///
    /// 設定はここで実際に使う形にそろえます（`APP_ENV=production` なら `Secure`）。
    pub fn new(store: impl SessionStore, config: SessionConfig) -> Self {
        Self {
            store: Arc::new(store),
            config: config.resolved(),
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
        self.config = config.resolved();
        self
    }
}

impl Middleware for StartSession {
    fn handle(&self, mut req: Request, next: Next) -> BoxFuture {
        let store = self.store.clone();
        // 設定は組み立てたときにそろえてある。ここではクローンだけ。
        let config = self.config.clone();

        Box::pin(async move {
            let key = signing_key()?;

            // 1. Cookie から ID を読む。署名が合わないものは無かったことにする。
            let incoming = req.cookie(&config.cookie).and_then(|raw| unsign(&raw, key));

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
            // 置き場所をこのセッションに結びつける。
            // `Auth::logout_other_devices()` が、同じ利用者のほかのセッションを消せるようになる。
            let session = session.with_store(store.clone());

            let id_before = session.id();
            req.set_session(session.clone());

            // 3. ハンドラへ。
            let response = next.run(req).await?;

            // 4. 要るときだけ保存する。
            //
            // 中身のクローン（`snapshot_for_save`）は、保存が要ると決まってから行う。
            // 読むだけのリクエストでクローンしても捨てるだけです。
            let id_after = session.id();
            // ID を作り直したかどうかは「古いファイルを消すか」の判定に使う。
            // `regenerate()` が `dirty` を立てるので、保存の判定には足しません。
            let id_changed = id_before != id_after;
            let saved = session.should_save();

            if saved {
                let data = session.snapshot_for_save();
                if id_changed {
                    // 古いほうは残さない。
                    // 消せなくても応答は返す（新しいほうは正しく保存できている）。
                    // 古い ID の Cookie はもう誰も持っていないので、致命的ではない。
                    let store = store.clone();
                    let old = id_before.clone();
                    match tokio::task::spawn_blocking(move || store.destroy(&old)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::error!("作り直す前のセッションを消せませんでした: {e}")
                        }
                        Err(_) => tracing::error!("作り直す前のセッションの削除が中断されました"),
                    }
                }
                if session.was_flushed() && data.is_empty() {
                    // ログアウトの経路。**消せなければ 500 にする。**
                    // 握り潰すと 200 を返しながら中身が残り、盗まれた Cookie が
                    // 期限まで使えてしまいます。
                    let store = store.clone();
                    let id = id_after.clone();
                    tokio::task::spawn_blocking(move || store.destroy(&id))
                        .await
                        .map_err(|_| Error::msg("セッションの削除が中断されました"))??;
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
                let cookie = Cookie::new(&config.cookie, sign(&id_after, key))
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

/// セッション Cookie の署名に使うラベル。
///
/// `APP_KEY` をそのまま使わず、用途ごとに別の鍵を作ります。
const SIGNING_LABEL: &str = "bengara:session";

/// 署名の鍵。`APP_KEY` から用途ごとに作ります。
///
/// **一度作ったら使い回します。** `APP_KEY` は起動後に変わらないので、
/// リクエストごとに HMAC-SHA256 を計算し直す意味がありません。
/// 鍵が設定されていないときの案内だけは、毎回その場で作ります（まれなので）。
fn signing_key() -> Result<&'static [u8]> {
    static KEY: OnceLock<Vec<u8>> = OnceLock::new();
    if let Some(found) = KEY.get() {
        return Ok(found.as_slice());
    }
    let derived = crate::config_registry::app_config().derived_key(SIGNING_LABEL)?;
    Ok(KEY.get_or_init(|| derived).as_slice())
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
///
/// # 確認を外す
///
/// | 外し方 | 見るもの |
/// |---|---|
/// | [`except_route`](Self::except_route) | ルートの名前。**こちらを勧めます** |
/// | [`except`](Self::except) | パス |
///
/// ```ignore
/// VerifyCsrfToken::new().except_route("api.*")
/// ```
///
/// ルート名で外すと、`routes/` でパスを変えてもずれません。
/// パスを書き写す形は、ルート表との二重管理になります。
pub struct VerifyCsrfToken {
    except: Vec<String>,
    except_routes: Vec<String>,
}

impl Default for VerifyCsrfToken {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyCsrfToken {
    /// 全部のパスで確かめる。
    pub fn new() -> Self {
        Self {
            except: Vec::new(),
            except_routes: Vec::new(),
        }
    }

    /// 確かめないパスを足す。末尾の `*` で前方一致にできます。
    ///
    /// ```ignore
    /// VerifyCsrfToken::new().except("/api/*")
    /// ```
    ///
    /// **ルート名で外せる [`except_route`](Self::except_route) のほうが、
    /// パスを変えてもずれません。** パスを書き写す形は、ルート表を直したときに
    /// 外しすぎ・外し漏れが静かに起きます。
    ///
    /// `*` は**区切り（`/`）も含めた残り全部**に当たります。
    /// つまり `/api/*` は `/api/`・`/api/posts`・`/api/posts/1` に当たり、
    /// `/apiX` には当たりません。
    ///
    /// `*` の直前が `/` でないとき（`/admin*` など）は、`/administration` のような
    /// 別のパスまで外してしまいます。断らずに通しますが、**警告を出します。**
    /// 前方一致そのものは使い道があるので禁止はせず、
    /// 書き間違いだけ気づけるようにしました。
    pub fn except(mut self, pattern: &str) -> Self {
        // 警告は登録のとき（起動時）に 1 回だけ出す。リクエストごとに調べない。
        if let Some(prefix) = pattern.strip_suffix('*') {
            if !prefix.is_empty() && !prefix.ends_with('/') {
                tracing::warn!(
                    "VerifyCsrfToken::except(\"{pattern}\") は {prefix} で始まる\
                     すべてのパスに当たります。`{prefix}/*` のつもりではありませんか"
                );
            }
        }
        self.except.push(pattern.to_string());
        self
    }

    /// 確かめない**ルート**を名前で足す。末尾の `*` で前方一致にできます。
    ///
    /// ```ignore
    /// VerifyCsrfToken::new().except_route("articles.*")
    /// ```
    ///
    /// `articles.*` は `articles.index`・`articles.store`・`articles.destroy` に
    /// 当たります。前方一致の規則は [`except`](Self::except) と同じです。
    ///
    /// **こちらを勧めます。** パスを書き写さないので、`routes/` でパスを変えても
    /// ずれません。
    ///
    /// **名前を付けていないルートは外せません。** ルート名が無いリクエストは、
    /// どの `except_route` にも当たりません（パスで外すなら
    /// [`except`](Self::except) を使ってください）。
    pub fn except_route(mut self, pattern: &str) -> Self {
        self.except_routes.push(pattern.to_string());
        self
    }

    fn is_excepted(&self, path: &str) -> bool {
        matches_any(&self.except, path)
    }

    /// ルート名で外す対象か。名前が無ければ当たりません。
    fn is_excepted_route(&self, name: Option<&str>) -> bool {
        match name {
            Some(name) => matches_any(&self.except_routes, name),
            None => false,
        }
    }
}

/// 一覧のどれかに当たるか。末尾の `*` は前方一致です。
fn matches_any(patterns: &[String], value: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => value.starts_with(prefix),
            None => value == pattern,
        })
}

/// セッションの中でトークンを入れておく場所。
pub(crate) const CSRF_KEY: &str = "__csrf_token";

impl Middleware for VerifyCsrfToken {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let safe = matches!(req.method(), "GET" | "HEAD" | "OPTIONS");
        // パスでもルート名でも外せる。どちらかに当たれば確かめない。
        let excepted = self.is_excepted(req.path()) || self.is_excepted_route(req.route_name());
        // ルートが無いなら守る対象も無い。確かめずに通して 404 / 405 を返させる。
        // 共通のミドルウェアはルートに当たらなかったリクエストにも掛かるので、
        // ここで見分けないと、無いページへの POST が 404 ではなく 419 になります。
        let no_route = !req.route_matched();

        Box::pin(async move {
            let Some(session) = req.try_session() else {
                return Err(Error::msg(
                    "CSRF の確認にはセッションが要ります。bootstrap/app.rs で \
                     StartSession を VerifyCsrfToken より前に登録してください",
                ));
            };

            // 守る対象のルートが無いなら、トークンを渡す相手もいない。
            // ここで `ensure_token` を呼ぶと、Cookie 無しの `GET /no-such-page` ごとに
            // セッションのファイルが1つ増えます（`put` で保存が要る状態になるため）。
            if no_route {
                return next.run(req).await;
            }
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
    fn アスタリスクは区切りの先まで当たる() {
        let mw = VerifyCsrfToken::new().except("/api/*");
        assert!(mw.is_excepted("/api/"), "区切りだけでも当たる");
        assert!(mw.is_excepted("/api/posts"));
        assert!(mw.is_excepted("/api/posts/1"), "さらに下も当たる");
        assert!(!mw.is_excepted("/apiX"), "区切りが違えば当たらない");
        assert!(!mw.is_excepted("/api"), "区切りが無ければ当たらない");
    }

    #[test]
    fn 区切りの無いアスタリスクも前方一致で当たる() {
        // 書き間違いの可能性があるので警告は出すが、動きは変えない。
        let mw = VerifyCsrfToken::new().except("/admin*");
        assert!(mw.is_excepted("/admin"));
        assert!(mw.is_excepted("/admin/users"));
        assert!(mw.is_excepted("/administration"), "警告つきで当たる");
        assert!(!mw.is_excepted("/adm"));
    }

    #[test]
    fn ルート名で確認を外せる() {
        let mw = VerifyCsrfToken::new()
            .except_route("articles.*")
            .except_route("webhook");

        // 前方一致。
        assert!(mw.is_excepted_route(Some("articles.index")));
        assert!(mw.is_excepted_route(Some("articles.store")));
        assert!(
            mw.is_excepted_route(Some("articles.")),
            "区切りだけでも当たる"
        );
        // 完全一致。
        assert!(mw.is_excepted_route(Some("webhook")));
        assert!(!mw.is_excepted_route(Some("webhooks")));
        assert!(
            !mw.is_excepted_route(Some("articles")),
            "`.` が無ければ外れる"
        );
        assert!(!mw.is_excepted_route(Some("posts.index")));

        // 名前を付けていないルートは外せない。
        assert!(!mw.is_excepted_route(None));
        // ルート名は見ても、パスは見ない。
        assert!(!mw.is_excepted("/articles"));
    }

    #[test]
    fn パスとルート名はどちらでも外せる() {
        let mw = VerifyCsrfToken::new()
            .except("/webhook/*")
            .except_route("api.*");

        // パスだけで当たる。
        assert!(mw.is_excepted("/webhook/stripe"));
        assert!(!mw.is_excepted_route(Some("webhook.stripe")));

        // ルート名だけで当たる。
        assert!(mw.is_excepted_route(Some("api.articles.store")));
        assert!(!mw.is_excepted("/api/articles"));

        // どちらにも当たらない。
        assert!(!mw.is_excepted("/articles"));
        assert!(!mw.is_excepted_route(Some("articles.store")));
    }

    #[test]
    fn 何も外さなければ全部確かめる() {
        let mw = VerifyCsrfToken::new();
        assert!(!mw.is_excepted("/anything"));
        assert!(!mw.is_excepted_route(Some("anything")));
        assert!(!mw.is_excepted_route(None));
    }

    #[test]
    fn セッションの鍵は用途ごとに分かれている() {
        // `APP_KEY` の生バイトではなく、ラベルを混ぜた鍵を使う。
        assert_eq!(SIGNING_LABEL, "bengara:session");
        let app_key = "a".repeat(64);
        let config = crate::AppConfig {
            name: "test".into(),
            env: "testing".into(),
            debug: false,
            url: "http://localhost".into(),
            key: app_key.clone(),
        };
        let session = config.derived_key(SIGNING_LABEL).unwrap();
        assert_ne!(session, app_key.as_bytes());
        assert_ne!(session, config.derived_key("bengara:signed-url").unwrap());
    }

    #[test]
    fn 本番ではsecureが強制される() {
        // 素の既定値（`resolved()` を通していない値）は false。
        let config = SessionConfig::default();
        assert!(!config.secure);

        // `resolved()` を通すと、本番のときだけ true になる。
        let is_production = crate::config_registry::app_config().is_production();
        assert_eq!(config.resolved().secure, is_production);

        // 自分で true にした値は、本番でなくても下げない。
        let explicit = SessionConfig {
            secure: true,
            ..SessionConfig::default()
        };
        assert!(explicit.resolved().secure);
    }

    #[test]
    fn 設定は組み立てたときにそろえる() {
        // リクエストごとに `resolved()` を呼ばないので、ここでそろっていること。
        // `secure` は `APP_ENV` で変わるため、環境に左右されない項目で確かめます
        // （`resolved()` の中身そのものは `本番ではsecureが強制される` が見ます）。
        let mw = StartSession::memory();
        assert_eq!(mw.config.cookie, "bengara_session");
        assert_eq!(mw.config.path, "/");
        assert_eq!(mw.config.lifetime_secs, 7200);

        // `with_config` でも同じようにそろえる。
        let mw = StartSession::memory().with_config(SessionConfig {
            cookie: "other".into(),
            path: "/app".into(),
            ..SessionConfig::default()
        });
        assert_eq!(mw.config.cookie, "other");
        assert_eq!(mw.config.path, "/app");
    }

    #[test]
    fn 署名の鍵は使い回す() {
        // 2 回目は HMAC を計算し直さず、同じ値を指す。
        match (signing_key(), signing_key()) {
            (Ok(a), Ok(b)) => assert!(a.as_ptr() == b.as_ptr(), "同じ鍵を指す"),
            // APP_KEY が無い環境。案内は毎回その場で作るので、どちらもエラー。
            (Err(_), Err(_)) => {}
            _ => panic!("鍵の判定が揺れた"),
        }
    }

    #[test]
    fn トークンを用意すると保存が要る状態になる() {
        // だからこそ、ルートに当たらないリクエストでは `ensure_token` を呼ばない。
        // 呼ぶと Cookie 無しの `GET /no-such-page` ごとにファイルが1つ増える。
        let session = Session::empty();
        assert!(!session.should_save(), "触る前は保存が要らない");
        ensure_token(&session);
        assert!(session.should_save(), "トークンを入れると保存が要る");
    }
}
