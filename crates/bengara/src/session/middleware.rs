//! セッションを読み書きするミドルウェアと、CSRF の確認。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

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
///
/// # 同じセッションのリクエストは直列になります
///
/// 1本のリクエストは、置き場所から読んだ中身を**マップごと**書き戻します。
/// 読みと書きのあいだに錠が無いと後勝ちになり、同時に来た2本のうち
/// 片方が入れた値が消えます。新しいセッションに画面と XHR が同時に来ると、
/// それぞれが**別の CSRF トークン**を作り、画面に埋め込んだほうが負けて、
/// 次の POST が 419 になりました。
///
/// そこで **ID ごとに錠を取り、読み込みから保存までを押さえます。**
/// 同じセッション ID のリクエストは順番に処理されます（Laravel も
/// セッションのファイルを錠で押さえます）。別のセッションどうしは待ちません。
/// 長くかかるハンドラは、同じ利用者の次のリクエストを待たせます。
pub struct StartSession {
    store: Arc<dyn SessionStore>,
    config: SessionConfig,
    /// 置き場所をどうやって作ったか。`with_config` で作り直せるか決めます。
    kind: StoreKind,
}

/// 置き場所の作り方。
///
/// 置き場所は「放っておいて消えるまでの秒数」を**作るときに焼き付けます**。
/// `with_config` で秒数が変わったら作り直さないと、Cookie の `Max-Age` と
/// サーバー側の期限が食い違います。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreKind {
    /// `file()` / `from_env()` が作った `FileStore`。
    File,
    /// `memory()` / `from_env()` が作った `MemoryStore`。
    Memory,
    /// `new()` で外から渡された。**作り直せません。**
    Given,
}

impl StartSession {
    /// 置き場所を指定して作る。
    ///
    /// 設定はここで実際に使う形にそろえます（`APP_ENV=production` なら `Secure`）。
    ///
    /// **`lifetime_secs` は置き場所にも伝えてください。** 外から渡した置き場所は
    /// [`with_config`](Self::with_config) では作り直せません。
    pub fn new(store: impl SessionStore, config: SessionConfig) -> Self {
        Self::of(Arc::new(store), config, StoreKind::Given)
    }

    /// 作り方つきで組み立てる（中だけで使います）。
    fn of(store: Arc<dyn SessionStore>, config: SessionConfig, kind: StoreKind) -> Self {
        Self {
            store,
            config: config.resolved(),
            kind,
        }
    }

    /// `storage/framework/sessions/` に置く（既定）。
    pub fn file() -> Self {
        let config = SessionConfig::default();
        let store = Arc::new(super::FileStore::default_path(config.lifetime_secs));
        Self::of(store, config, StoreKind::File)
    }

    /// プロセスのメモリに置く。**テスト専用です。**
    pub fn memory() -> Self {
        let config = SessionConfig::default();
        let store = Arc::new(super::MemoryStore::new(config.lifetime_secs));
        Self::of(store, config, StoreKind::Memory)
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
            "memory" | "array" => {
                let store = Arc::new(super::MemoryStore::new(config.lifetime_secs));
                Self::of(store, config, StoreKind::Memory)
            }
            _ => {
                let store = Arc::new(super::FileStore::default_path(config.lifetime_secs));
                Self::of(store, config, StoreKind::File)
            }
        }
    }

    /// 設定を差し替える。
    ///
    /// `lifetime_secs` を変えたときは、**組み込みの置き場所も作り直します。**
    /// 置き場所は秒数を作るときに焼き付けるので、作り直さないと Cookie の
    /// `Max-Age` だけが変わり、サーバー側の期限が食い違います。
    ///
    /// [`new`](Self::new) で外から渡した置き場所は作り直せません。
    /// 食い違うときは警告を出します。
    pub fn with_config(mut self, config: SessionConfig) -> Self {
        let config = config.resolved();
        if config.lifetime_secs != self.config.lifetime_secs {
            match self.kind {
                StoreKind::File => {
                    self.store = Arc::new(super::FileStore::default_path(config.lifetime_secs));
                }
                StoreKind::Memory => {
                    self.store = Arc::new(super::MemoryStore::new(config.lifetime_secs));
                }
                StoreKind::Given => tracing::warn!(
                    "with_config の lifetime_secs を {} 秒に変えましたが、\
                     StartSession::new に渡した置き場所は作り直せません。\
                     置き場所にも同じ秒数を渡してください",
                    config.lifetime_secs
                ),
            }
        }
        self.config = config;
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

            // 置き場所から**実際に読めたか**。署名は合うが中身が無い Cookie
            // （期限切れ・ログアウト後）を持っている人に、保存していない新しい ID を
            // 毎リクエスト配り続けないための目印です。
            let mut loaded = false;

            // 1.5. 同じ ID のリクエストを直列にする錠を取る。
            //
            // **読み込みから保存までを押さえます。** 押さえないと、同時に来た2本が
            // それぞれマップ全体を読んで書き戻すので後勝ちになり、片方の値が消えます。
            // 新しく作る ID（Cookie が無い・読めなかったとき）は誰とも重ならないので、
            // Cookie から読めた ID のときだけ取ります。
            let id_lock = match &incoming {
                Some(id) => Some(IdLock::acquire(id).await),
                None => None,
            };

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
                            loaded = true;
                            let (data, old_flash) = Session::split_flash(stored);
                            Session::new(id, data, old_flash).mark_loaded()
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
            let mut after = after_request(saved, loaded);

            if after == AfterRequest::Save {
                let data = session.snapshot_for_save();

                // 4.0. 処理中に、この利用者のセッションが別の所から切られていないか。
                //
                // `logout_other_devices` / `logout_user` はファイルを消すだけです。
                // その瞬間に処理中だったこのリクエストが保存すると、消したはずの
                // セッションが同じ ID で復活します。置き場所が持つ「切った」印が
                // **このセッションがログインした時刻**より新しければ、保存せずに
                // **消す側**に倒します。比べるのはログインの時刻（ミリ秒）です。
                // 読み込んだ時刻で比べると、切った直後に別人として同じ利用者 ID で
                // ログインし直したセッションまで同じ秒の中で巻き込みます。
                //
                // **ID を作り直したかどうかは見ません。** `regenerate()` は ID だけを
                // 変えて `AUTH_AT_KEY` を引き継ぐので、「ログインし直し」の代わりに
                // なりません。見ないでおくと、切っている最中に `regenerate()` を
                // 通ったリクエストが、切ったはずのセッションを新しい ID で
                // 生き残らせます。本物のログインし直し（`Auth::login_using_id`）は
                // `AUTH_AT_KEY` を更新するので、`at > logged_in_at` の比較が守ります。
                let revoke_check = if loaded {
                    session.get(crate::auth::AUTH_ID_KEY).map(|user| {
                        // 印より前にログインしたものだけが対象。時刻が無い古い
                        // セッションは 0 として、印があれば切る側に倒す。
                        let logged_in_at = session
                            .get(crate::auth::AUTH_AT_KEY)
                            .and_then(|v| v.parse::<i64>().ok())
                            .unwrap_or(0);
                        (user, logged_in_at)
                    })
                } else {
                    None
                };
                let flushed = session.was_flushed() && data.is_empty();
                // 作り直す前の ID。切られたときも、保存したときも残しません。
                let old = id_changed.then(|| id_before.clone());

                let store = store.clone();
                let id = id_after.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    save_session(&*store, &id, &data, revoke_check, flushed, old.as_deref())
                })
                .await
                .map_err(|_| Error::msg("セッションの保存が中断されました"))??;

                if outcome == SaveOutcome::Revoked {
                    tracing::info!("切られたセッションなので保存しません");
                    after = AfterRequest::Nothing;
                }
            }

            // 4.5. 保存は要らないが、置き場所から読めていたとき。
            //
            // **置き場所の期限も延ばします。** `expires_at` は「最後に書いたとき」から
            // 数えるので、延ばさないと GET の画面を見て回っていた人が、
            // ログインや POST から `SESSION_LIFETIME` で突然ログアウトされます。
            // Cookie の `Max-Age` を延ばしても、サーバー側が切れていれば読めません。
            //
            // 毎リクエスト書かないよう、**延ばすかどうかは置き場所が決めます**
            // （`SessionStore::touch`。残りが半分を切ったときだけ書く）。
            // 延ばせなくても応答は返します。致命的ではありません。
            if after == AfterRequest::Touch {
                let store = store.clone();
                let id = id_after.clone();
                match tokio::task::spawn_blocking(move || store.touch(&id)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!("セッションの期限を延ばせませんでした: {e}"),
                    Err(_) => tracing::warn!("セッションの期限の延長が中断されました"),
                }
            }

            // 4.9. 錠を放す。保存も期限の延長も終わっているので、
            // Cookie を組み立てるあいだ押さえておく必要はありません。
            drop(id_lock);

            // 5. Cookie を返す。
            //
            // 保存したときと、**置き場所から実際に読めたとき**（期限を延ばす）だけです。
            // 「署名の合う Cookie を持っていた」だけで配ると、置き場所に中身が無い人に
            // 保存していない新しい ID を毎リクエスト配り続けます。
            if after != AfterRequest::Nothing {
                let cookie = Cookie::new(&config.cookie, sign(&id_after, key))
                    .with_path(&config.path)
                    .with_max_age(i64::try_from(config.lifetime_secs).unwrap_or(i64::MAX))
                    .with_secure(config.secure)
                    .with_http_only(true)
                    .with_same_site(config.same_site);
                return Ok(response.with_cookie(&cookie));
            }
            Ok(response)
        })
    }
}

/// 保存の経路で何をしたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveOutcome {
    /// 保存した（ログアウトの経路では消した）。
    Done,
    /// 処理中に切られていたので、保存せずに消した。
    Revoked,
}

/// 「切られていないか確認 → 保存または削除」をひとまとめに行う。
///
/// **確認と保存を別の `spawn_blocking` に分けません。** 分けると、
/// スレッドプールを往復するあいだに `logout_user` が走り、消したはずの
/// セッションが復活します。1 つにまとめれば、窓は同じスレッドの中の
/// 2 操作ぶんに縮み、往復も 1 回減ります。
///
/// `revoke_check` は `(利用者 ID, ログインした時刻のミリ秒)` です。
/// 置き場所から読めていない（このリクエストで作った）セッションでは `None` にします。
///
/// `old` は ID を作り直したときの**作り直す前の** ID です。切られたときも
/// 保存したときも残しません。残すと、切ったはずのセッションが古い ID で
/// 生き残ります。「切った印」の判定にも使います（下のコメントを参照）。
fn save_session(
    store: &dyn SessionStore,
    id: &str,
    data: &BTreeMap<String, String>,
    revoke_check: Option<(String, i64)>,
    flushed: bool,
    old: Option<&str>,
) -> Result<SaveOutcome> {
    if let Some(old) = old {
        // 消せなくても応答は返す（新しいほうは正しく保存できている）。
        // 古い ID の Cookie はもう誰も持っていないので、致命的ではない。
        if let Err(e) = store.destroy(old) {
            tracing::error!("作り直す前のセッションを消せませんでした: {e}");
        }
    }
    if let Some((user, logged_in_at)) = revoke_check {
        // 印が効いているかを、**作り直す前と後の両方の ID** で見ます。
        //
        // `logout_other_devices()` は「いま操作している ID」を印の除外に書きます。
        // そのあと同じリクエストで `regenerate()` を呼ぶと、新しい ID は除外に
        // 当たらないので印が効いてしまい、**残すはずの自分のセッションが消えます**
        // （呼んだ本人がログアウトされる）。作り直す前の ID は除外に当たって
        // `None` が返るので、**両方が切られているときだけ**消す形にすれば、
        // この取り違えが起きません。
        //
        // 別の所からの `logout_user`（除外なし）では両方に印が効くので、
        // 塞ぎたかった取りこぼし（処理中のリクエストが復活させる）はそのままです。
        let revoked = |target: &str| {
            store
                .revoked_at(&user, target)
                .is_some_and(|at| at > logged_in_at)
        };
        if revoked(id) && old.is_none_or(revoked) {
            store.destroy(id)?;
            return Ok(SaveOutcome::Revoked);
        }
    }
    if flushed {
        // ログアウトの経路。**消せなければ 500 にする。**
        // 握り潰すと 200 を返しながら中身が残り、盗まれた Cookie が
        // 期限まで使えてしまいます。
        store.destroy(id)?;
    } else {
        store.write(id, data)?;
    }
    Ok(SaveOutcome::Done)
}

/// セッション ID ごとの錠を覚えておく地図。
///
/// 中の `std::sync::Mutex` は**地図を触るあいだだけ**持ちます（`await` を越えません）。
/// 待つのは中身の `tokio::sync::Mutex` のほうです。tokio の錠を待つあいだは
/// ワーカースレッドを塞がないので、同じセッションの2本目が来ても他の人は止まりません。
fn id_locks() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 1つのセッション ID を押さえている間だけ生きる錠。
///
/// 落ちるときに、待っている人がいなければ地図から片付けます。
/// 片付けないと、セッション ID の数だけ組が残り続けます。
struct IdLock {
    id: String,
    /// 落とす順を決めたいので `Option` にしています（[`Drop`] を参照）。
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl IdLock {
    /// その ID の錠を取る。すでに誰かが持っていれば、放されるまで待ちます。
    async fn acquire(id: &str) -> Self {
        let lock_arc = {
            let mut map = id_locks().lock().unwrap_or_else(|e| e.into_inner());
            map.entry(id.to_string()).or_default().clone()
        };
        // 地図の錠はここで放してある。`await` を越えて持たない。
        //
        // **`await` より前に `IdLock` を作ります。** 地図に組を入れてから
        // `await` すると、待っているあいだに外側の future が落とされたときに
        // `IdLock` がまだ無く、`Drop` も走りません。組（64 文字の文字列と `Arc`）が
        // プロセス全体の地図に残り続けます。先に作っておけば、落とされても
        // `Drop` が片付けます。
        let mut lock = Self {
            id: id.to_string(),
            guard: None,
        };
        // `lock_owned` は `Arc` を**受け取ります**。複製を渡さないことが、
        // `Drop` の「地図のぶんだけなら待っている人はいない」の根拠です。
        lock.guard = Some(lock_arc.lock_owned().await);
        lock
    }
}

impl Drop for IdLock {
    fn drop(&mut self) {
        // **先に錠を放します。** 放す前に数えると、自分が持っているぶんで
        // 必ず 2 以上になり、いつまでも片付けられません。
        self.guard = None;
        let mut map = id_locks().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(found) = map.get(&self.id) {
            // 地図のぶんだけなら、待っている人はいない。
            // 待っている人は地図の錠を取ってから複製するので、ここで数え漏らしません。
            if Arc::strong_count(found) == 1 {
                map.remove(&self.id);
            }
        }
    }
}

/// リクエストの終わりにすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterRequest {
    /// 何もしない。Cookie も返しません。
    Nothing,
    /// 期限だけ延ばして、Cookie を返す。
    Touch,
    /// 保存して、Cookie を返す。
    Save,
}

/// 保存が要るか・置き場所から読めたかで、終わりにすることを決める。
///
/// | 保存が要る | 読めた | すること |
/// |---|---|---|
/// | はい | - | 保存して Cookie |
/// | いいえ | はい | 期限を延ばして Cookie |
/// | いいえ | いいえ | 何もしない |
///
/// **「Cookie を持っていた」ではなく「置き場所から読めた」で決めます。**
/// 署名は合うが中身が無い Cookie（期限切れ・ログアウト後）の人に Cookie を返すと、
/// 保存していない新しい ID を毎リクエスト配り続けることになります。
fn after_request(saved: bool, loaded: bool) -> AfterRequest {
    match (saved, loaded) {
        (true, _) => AfterRequest::Save,
        (false, true) => AfterRequest::Touch,
        (false, false) => AfterRequest::Nothing,
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
/// 探す順は、`X-CSRF-TOKEN` ヘッダー → フォームの `_token` です。
///
/// `_token` を先に見ると、ヘッダーで送ってくる SPA の JSON 本文（最大 2 MiB）を
/// トークンを探すためだけに毎回解析することになります。ハンドラの `req.json()` と
/// 二重になるので、ヘッダーが無いときだけ本文を見ます。
fn token_from_request(req: &Request) -> String {
    if let Some(token) = req.header("x-csrf-token") {
        if !token.is_empty() {
            return token.to_string();
        }
    }
    req.input("_token").unwrap_or_default()
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
    fn トークンはヘッダーを先に見る() {
        // ヘッダーがあれば本文は見ない（JSON を解析しない）。
        let req = Request::new("post", "/")
            .with_headers(vec![
                ("x-csrf-token".into(), "from-header".into()),
                ("content-type".into(), "application/json".into()),
            ])
            .with_body(b"{\"_token\":\"from-body\"}".to_vec());
        assert_eq!(token_from_request(&req), "from-header");

        // ヘッダーが無ければ本文の `_token`。
        let req = Request::new("post", "/")
            .with_headers(vec![(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )])
            .with_body(b"_token=from-form".to_vec());
        assert_eq!(token_from_request(&req), "from-form");

        // どちらも無ければ空。
        assert_eq!(token_from_request(&Request::new("post", "/")), "");
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
    fn 置き場所に無いcookieには何も返さない() {
        // 署名は合うが中身が無い Cookie（期限切れ・ログアウト後）の人に、
        // 保存していない新しい ID を毎リクエスト配らない。
        assert_eq!(after_request(false, false), AfterRequest::Nothing);
        // 読めたときは、読むだけのリクエストでも期限を延ばす。
        assert_eq!(after_request(false, true), AfterRequest::Touch);
        // 保存が要るときは保存する。
        assert_eq!(after_request(true, false), AfterRequest::Save);
        assert_eq!(after_request(true, true), AfterRequest::Save);
    }

    #[test]
    fn with_configの秒数は置き場所にも届く() {
        use std::collections::BTreeMap;

        // 0 秒にすると、置き場所に書いた直後から読めない。
        let mw = StartSession::memory().with_config(SessionConfig {
            lifetime_secs: 0,
            ..SessionConfig::default()
        });
        assert_eq!(mw.config.lifetime_secs, 0);
        mw.store.write("abc", &BTreeMap::new()).unwrap();
        assert!(
            mw.store.read("abc").unwrap().is_none(),
            "置き場所を作り直している"
        );

        // 秒数を変えなければ、既定（7200 秒）のまま読める。
        let mw = StartSession::memory();
        mw.store.write("abc", &BTreeMap::new()).unwrap();
        assert!(mw.store.read("abc").unwrap().is_some());

        // `new` で渡した置き場所は作り直さない（警告だけ）。設定は変わる。
        let mw = StartSession::new(
            crate::session::MemoryStore::new(7200),
            SessionConfig::default(),
        )
        .with_config(SessionConfig {
            lifetime_secs: 0,
            ..SessionConfig::default()
        });
        assert_eq!(mw.config.lifetime_secs, 0);
        mw.store.write("abc", &BTreeMap::new()).unwrap();
        assert!(
            mw.store.read("abc").unwrap().is_some(),
            "外から渡した置き場所はそのまま"
        );
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

    #[tokio::test]
    async fn 同じidの錠は順番に取れる() {
        // 読み込みから保存までを押さえないと、同時に来た2本が
        // それぞれマップ全体を書き戻して後勝ちになり、片方の値が消える。
        let id = format!("test-{}", crypto::random_token());

        let first = IdLock::acquire(&id).await;
        // 同じ ID は待たされる（錠を持っているあいだは取れない）。
        let waiting = {
            let id = id.clone();
            tokio::spawn(async move { IdLock::acquire(&id).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "放すまで取れない");

        // 別の ID は待たない。
        let other = IdLock::acquire(&format!("other-{}", crypto::random_token())).await;
        drop(other);

        drop(first);
        let second = waiting.await.expect("待っていたほうが取れる");
        drop(second);

        // 使い終わった錠は地図に残さない。
        assert!(
            !id_locks()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&id),
            "片付ける"
        );
    }

    /// 認証 ID とログイン時刻を入れた中身を作る。
    fn logged_in(at: i64) -> BTreeMap<String, String> {
        let mut data = BTreeMap::new();
        data.insert(crate::auth::AUTH_ID_KEY.to_string(), "7".to_string());
        data.insert(crate::auth::AUTH_AT_KEY.to_string(), at.to_string());
        data
    }

    #[test]
    fn 切られていなければ保存する() {
        let store = crate::session::MemoryStore::new(60);
        let data = logged_in(1_000);

        let outcome =
            save_session(&store, "aaa", &data, Some(("7".into(), 1_000)), false, None).unwrap();
        assert_eq!(outcome, SaveOutcome::Done);
        assert!(store.read("aaa").unwrap().is_some());
    }

    #[test]
    fn 切られていれば保存せずに消す() {
        let store = crate::session::MemoryStore::new(60);
        let data = logged_in(1_000);
        store.write("aaa", &data).unwrap();
        // ログインより後に切られた印。
        store.mark_revoked("7", None);

        let outcome =
            save_session(&store, "aaa", &data, Some(("7".into(), 1_000)), false, None).unwrap();
        assert_eq!(outcome, SaveOutcome::Revoked);
        assert!(store.read("aaa").unwrap().is_none(), "復活させない");
    }

    #[test]
    fn idを作り直しても切られたら生き残らない() {
        // `regenerate()` は ID だけを変えて `AUTH_AT_KEY` を引き継ぐので、
        // 「ID が変わった＝ログインし直し」ではない。「ID が変わったら見ない」に
        // していたころは、切っている最中に `regenerate()` を通ったリクエストが
        // 切ったはずのセッションを新しい ID で生き残らせた。
        let store = crate::session::MemoryStore::new(60);
        let data = logged_in(1_000);
        store.write("old", &data).unwrap();
        store.mark_revoked("7", None);

        let outcome = save_session(
            &store,
            "new",
            &data,
            Some(("7".into(), 1_000)),
            false,
            Some("old"),
        )
        .unwrap();
        assert_eq!(outcome, SaveOutcome::Revoked);
        assert!(store.read("new").unwrap().is_none(), "新しい ID で残らない");
        assert!(store.read("old").unwrap().is_none(), "古い ID も残らない");
    }

    #[test]
    fn 他の端末を切った後にidを作り直しても自分は残る() {
        // `logout_other_devices()` は「いま操作している ID」を印の除外に書く。
        // そのあと同じリクエストで `regenerate()` を呼ぶと、新しい ID は除外に
        // 当たらない。新しい ID だけを見ていたころは、呼んだ本人のセッションが
        // 消えていた（＝自分がログアウトされる）。
        let store = crate::session::MemoryStore::new(60);
        let data = logged_in(1_000);
        store.write("old", &data).unwrap();
        // 除外は「作り直す前の ID」。
        store.mark_revoked("7", Some("old"));

        let outcome = save_session(
            &store,
            "new",
            &data,
            Some(("7".into(), 1_000)),
            false,
            Some("old"),
        )
        .unwrap();
        assert_eq!(outcome, SaveOutcome::Done, "自分のセッションは残す");
        assert!(store.read("new").unwrap().is_some());
    }

    #[test]
    fn 切った後にログインし直したセッションは残る() {
        // 本物のログインし直し（`Auth::login_using_id`）は `AUTH_AT_KEY` を
        // 更新する。印より後にログインしたものは巻き込まない。
        let store = crate::session::MemoryStore::new(60);
        store.mark_revoked("7", None);
        let later = crate::support::time::now_millis() + 1_000;
        let data = logged_in(later);

        let outcome =
            save_session(&store, "new", &data, Some(("7".into(), later)), false, None).unwrap();
        assert_eq!(outcome, SaveOutcome::Done);
        assert!(store.read("new").unwrap().is_some());
    }

    #[test]
    fn ログアウトの経路は書かずに消す() {
        let store = crate::session::MemoryStore::new(60);
        store.write("aaa", &logged_in(1_000)).unwrap();

        let outcome = save_session(&store, "aaa", &BTreeMap::new(), None, true, None).unwrap();
        assert_eq!(outcome, SaveOutcome::Done);
        assert!(store.read("aaa").unwrap().is_none());
    }

    #[test]
    fn 作り直す前のidは保存のときも消す() {
        let store = crate::session::MemoryStore::new(60);
        let data = logged_in(1_000);
        store.write("old", &data).unwrap();

        let outcome = save_session(&store, "new", &data, None, false, Some("old")).unwrap();
        assert_eq!(outcome, SaveOutcome::Done);
        assert!(store.read("new").unwrap().is_some());
        assert!(store.read("old").unwrap().is_none());
    }

    #[tokio::test]
    async fn 待っているあいだに落とされても地図に組が残らない() {
        // 地図に組を入れてから `await` していたころは、待っているあいだに
        // 外側の future が落とされると `IdLock` がまだ無く、`Drop` も走らない。
        // 組（64 文字の文字列と `Arc`）がプロセス全体の地図に残り続けた。
        let id = format!("test-{}", crypto::random_token());

        let held = IdLock::acquire(&id).await;
        let waiting = {
            let id = id.clone();
            tokio::spawn(async move { IdLock::acquire(&id).await })
        };
        // 待っている側が錠を待つところまで進める。
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "まだ待っている");

        // 錠を放してから、待っている側を**動かさずに**落とす。
        // ここに `await` が無いので、1 スレッドの runtime では進めない。
        drop(held);
        waiting.abort();
        let _ = waiting.await;

        assert!(
            !id_locks()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&id),
            "落とされた側の組も片付く"
        );
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
