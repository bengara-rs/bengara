//! セッションを読み書きするミドルウェアと、CSRF の確認。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use super::{Session, SessionStore};
use crate::error::{Error, Result};
use crate::http::{BoxFuture, Cookie, Middleware, Next, Request, Response, SameSite};
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
/// 同じセッション ID のリクエストは順番に処理されます。別のセッションどうしは
/// 待ちません。長くかかるハンドラは、同じ利用者の次のリクエストを待たせます。
///
/// Laravel の既定とは違います。Laravel がファイルに錠を掛けるのは読み書きの
/// 瞬間だけで、リクエスト全体を順番にするのは、ルートに `block()` を付けたときの
/// 任意の機能です。bengara はいつも順番にします。
///
/// **錠はプロセスの中だけです。** プロセスを 2 つ以上動かすと、別々のプロセスに
/// 入った同じセッションのリクエストは並んで走り、後から保存したほうが勝ちます。
///
/// # ルートに当たらなかったリクエストは「読むだけ」です
///
/// 共通のミドルウェアは、静的ファイル・404・405 にも掛かります。そこで毎回
/// 錠を取って書き戻すと、画面 1 枚と CSS・画像 20 本が 1 本ずつになり、
/// まだ画面に出していない flash が CSS を返しただけで消えます。
/// `req.route_matched()` が偽のときは、次のようにします。
///
/// | こと | ルートあり | ルートなし |
/// |---|---|---|
/// | ID の錠 | 読み込みから保存まで | 取らない（書き換えたときだけ、保存の瞬間に取る） |
/// | 期限の延長 | する | しない |
/// | 持ち越しの flash | 読めて、消費する | 読めるが、**消費しない** |
/// | Cookie | 返す | 書き換えたときだけ返す |
///
/// セッションそのものは載せるので、`req.session()` と `req.auth()` は使えます。
/// ミドルウェアが書き換えたときは、**置き場所を読み直して、変えた鍵だけを
/// 当ててから**保存します（錠を持たずに読んだ中身を丸ごと書き戻すと、
/// そのあいだに保存された別のリクエストの値を消してしまうため）。
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
    ///
    /// 知らない `SESSION_DRIVER`（`redis` や書き間違い）は、**警告を出してから**
    /// `file` にします。
    pub fn from_env() -> Self {
        let minutes: u64 = crate::env("SESSION_LIFETIME", 120u64);
        let config = SessionConfig {
            lifetime_secs: minutes.saturating_mul(60).max(60),
            ..SessionConfig::default()
        };
        let driver: String = crate::env("SESSION_DRIVER", "file");
        Self::from_driver(&driver, config)
    }

    /// 置き場所の名前から作る（`from_env` の中身）。
    fn from_driver(driver: &str, config: SessionConfig) -> Self {
        match driver {
            "memory" | "array" => {
                let store = Arc::new(super::MemoryStore::new(config.lifetime_secs));
                Self::of(store, config, StoreKind::Memory)
            }
            other => {
                // 黙って file にすると、`redis` のつもりで動かしていても気づけない。
                // 文面は `CACHE_DRIVER` とそろえてあります。
                if other != "file" {
                    tracing::warn!("SESSION_DRIVER `{other}` は知りません。file を使います");
                }
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
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let store = self.store.clone();
        // 設定は組み立てたときにそろえてある。ここではクローンだけ。
        let config = self.config.clone();

        Box::pin(async move {
            let key = signing_key()?;
            serve(store, config, key, req, next).await
        })
    }
}

/// 1 リクエストぶんの本体。
///
/// 署名の鍵を引数で受け取ります。`APP_KEY` を設定していないテストからも、
/// 読み込みから Cookie までを通しで確かめられるようにするためです。
async fn serve(
    store: Arc<dyn SessionStore>,
    config: SessionConfig,
    key: &[u8],
    mut req: Request,
    next: Next,
) -> Result<Response> {
    // 1. Cookie から ID を読む。署名が合わないものは無かったことにする。
    let raw = req.cookie(&config.cookie);
    // 署名が合わなくても「この名前の Cookie は持っていた」。
    // ログアウトのあとで、消す Cookie を返すかどうかに使います。
    let had_cookie = raw.is_some();
    let incoming = raw.and_then(|raw| unsign(&raw, key));

    // ルートに当たったか。当たらなかったリクエスト（静的ファイル・404・405）は
    // 「読むだけ」として扱います（`StartSession` の説明を参照）。
    let matched = req.route_matched();

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
    //
    // **ルートに当たらなかったときは取りません。** 取ると、同じブラウザの
    // 画面 1 枚と CSS・画像の 20 本が 1 本ずつになります。
    let mut id_lock = match &incoming {
        Some(id) if matched => Some(IdLock::acquire(id).await),
        _ => None,
    };

    // ルートに当たらなかったときに読んだ、**書き換える前の**中身。
    // あとで保存が要ると分かったときに、「どの鍵を変えたか」を出すのに使います。
    let mut base: Option<BTreeMap<String, String>> = None;

    // 2. 置き場所から中身を読む。
    //
    // ルートに当たったときは、**同じ読みで期限も延ばします**
    // （`read_and_touch`。残りが半分を切ったときだけ書く）。
    // `expires_at` は「最後に書いたとき」から数えるので、延ばさないと GET の
    // 画面を見て回っていた人が、ログインや POST から `SESSION_LIFETIME` で
    // 突然ログアウトされます。終わりに `touch` を別に呼んでいたころは、
    // 期限を知るためだけに同じファイルをもう 1 回読んで解析していました。
    //
    // ルートに当たらなかったときは読むだけです。CSS を配るたびに期限を
    // 延ばす必要はありません。
    let session = match incoming {
        Some(id) => {
            let found = {
                let store = store.clone();
                let id = id.clone();
                tokio::task::spawn_blocking(move || {
                    if matched {
                        store.read_and_touch(&id)
                    } else {
                        store.read(&id)
                    }
                })
                .await
                .map_err(|_| Error::msg("セッションの読み込みが中断されました"))??
            };
            match found {
                Some(stored) => {
                    loaded = true;
                    if !matched {
                        base = Some(stored.clone());
                    }
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
    // ルートに当たらなかったときは、**書き換えたときだけ**保存する。
    // 持ち越しの flash を抱えているだけでは保存しません（保存すると消費する）。
    let saved = if matched {
        session.should_save()
    } else {
        session.is_dirty()
    };
    let mut after = after_request(saved, loaded, matched);

    if after == AfterRequest::Save {
        // ルートに当たらなかったときは、持ち越しの flash を残す形で写し取る。
        let data = if matched {
            session.snapshot_for_save()
        } else {
            session.snapshot_keeping_flash()
        };

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
        //
        // `Auth::logout_other_devices()` も、**呼んだ本人の** `AUTH_AT_KEY` を
        // 印より新しい時刻に進めます。本人のセッションは、そのあと ID を
        // 何度作り直しても、この比較で残ります。
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

        // 4.1. ルートに当たらなかったリクエストが書き換えたとき。
        //
        // 錠を持たずに読んだ中身なので、丸ごと書き戻すと、そのあいだに
        // 保存された別のリクエストの値（作ったばかりの CSRF トークンなど）を
        // 消します。**ここで錠を取り、置き場所を読み直して、変えた鍵だけを
        // 当てます**（`save_merged`）。ハンドラのあいだは押さえていないので、
        // 待たせるのは保存の一瞬だけです。
        //
        // 中身を捨てたとき（ログアウト）は消すだけなので、読み直しません。
        let merge = if flushed { None } else { base.take() };
        if merge.is_some() {
            id_lock = Some(IdLock::acquire(&id_before).await);
        }

        let store = store.clone();
        let id = id_after.clone();
        let before = id_before.clone();
        let outcome = tokio::task::spawn_blocking(move || match merge {
            Some(base) => save_merged(
                &*store,
                &before,
                &base,
                &id,
                &data,
                revoke_check,
                old.as_deref(),
            ),
            None => save_session(&*store, &id, &data, revoke_check, flushed, old.as_deref()),
        })
        .await
        .map_err(|_| Error::msg("セッションの保存が中断されました"))??;

        match outcome {
            SaveOutcome::Done => {}
            // ログアウト。置き場所には何も残していないので、新しい ID は配らない。
            // ブラウザに残った古い Cookie は、持っていたときだけ消す。
            SaveOutcome::Destroyed => {
                after = if had_cookie {
                    AfterRequest::Forget
                } else {
                    AfterRequest::Nothing
                };
            }
            SaveOutcome::Revoked => {
                tracing::info!("切られたセッションなので保存しません");
                after = AfterRequest::Nothing;
            }
            SaveOutcome::Gone => {
                tracing::debug!("処理中にセッションが消えたので、書き換えを保存しません");
                after = AfterRequest::Nothing;
            }
        }
    }

    // 4.9. 錠を放す。保存は終わっているので、
    // Cookie を組み立てるあいだ押さえておく必要はありません。
    drop(id_lock);

    // 5. Cookie を返す。
    //
    // 保存したときと、**ルートに当たって置き場所から実際に読めたとき**
    // （Cookie の期限を延ばす）だけです。
    // 「署名の合う Cookie を持っていた」だけで配ると、置き場所に中身が無い人に
    // 保存していない新しい ID を毎リクエスト配り続けます。
    match after {
        AfterRequest::Nothing => Ok(response),
        AfterRequest::Touch | AfterRequest::Save => {
            let cookie = Cookie::new(&config.cookie, sign(&id_after, key))
                .with_path(&config.path)
                .with_max_age(i64::try_from(config.lifetime_secs).unwrap_or(i64::MAX))
                .with_secure(config.secure)
                .with_http_only(true)
                .with_same_site(config.same_site);
            Ok(response.with_cookie(&cookie))
        }
        // ログアウトした。同じ名前・同じパスの Cookie を、期限切れで上書きする。
        //
        // 以前は、置き場所から消したあとも**保存していない新しい ID** の Cookie を
        // 返していました。次のリクエストは「署名は合うが中身が無い」扱いになり、
        // 錠と空振りの読みが 1 回ずつ余計に走ります。Laravel は空のセッションを
        // 必ず保存して新しい ID を返しますが、bengara は空のセッションを
        // 置き場所に作らないので、Cookie のほうを消します。
        AfterRequest::Forget => {
            let cookie = Cookie::removal(&config.cookie)
                .with_path(&config.path)
                .with_secure(config.secure)
                .with_http_only(true)
                .with_same_site(config.same_site);
            Ok(response.with_cookie(&cookie))
        }
    }
}

/// 保存の経路で何をしたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveOutcome {
    /// 保存した。
    Done,
    /// ログアウトの経路。書かずに消した。
    Destroyed,
    /// 処理中に切られていたので、保存せずに消した。
    Revoked,
    /// ルートに当たらなかったリクエストが保存しようとしたが、読み直したら
    /// 置き場所から消えていた（ログアウト・期限切れなど）。書いていない。
    Gone,
}

/// ルートに当たらなかったリクエストの書き換えを保存する。
///
/// そのリクエストは錠を持たずに読んでいます。読んだ中身（`base`）と保存したい中身
/// （`ours`）を比べ、**変えた鍵だけ**を、いま置き場所にある中身に当てます。
/// 呼ぶ側が ID の錠を取ってから呼びます。
///
/// 読み直して無ければ書きません。そのあいだにログアウトしたか、期限が切れたか、
/// 別のリクエストが ID を作り直しています。書くと、消したセッションが復活します。
///
/// 引数の `id_before` は読んだときの ID、`id` は保存する ID です
/// （作り直していなければ同じ）。残りは [`save_session`] と同じです。
fn save_merged(
    store: &dyn SessionStore,
    id_before: &str,
    base: &BTreeMap<String, String>,
    id: &str,
    ours: &BTreeMap<String, String>,
    revoke_check: Option<(String, i64)>,
    old: Option<&str>,
) -> Result<SaveOutcome> {
    let Some(fresh) = store.read(id_before)? else {
        return Ok(SaveOutcome::Gone);
    };
    let merged = merge_changes(base, ours, fresh);
    save_session(store, id, &merged, revoke_check, false, old)
}

/// `base` から `ours` への変更（入れた・変えた・消した鍵）を `fresh` に当てる。
///
/// 触っていない鍵は `fresh` の値を残します。同じ鍵を両方が変えていたら、
/// あとから保存するこちらが勝ちます。
fn merge_changes(
    base: &BTreeMap<String, String>,
    ours: &BTreeMap<String, String>,
    mut fresh: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for (key, value) in ours {
        if base.get(key) != Some(value) {
            fresh.insert(key.clone(), value.clone());
        }
    }
    for key in base.keys() {
        if !ours.contains_key(key) {
            fresh.remove(key);
        }
    }
    fresh
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
        return Ok(SaveOutcome::Destroyed);
    }
    store.write(id, data)?;
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
        // **確保は地図の錠を取る前に済ませます。** 地図はプロセス全体で 1 つなので、
        // Cookie つきの全リクエストがここを通ります。錠の中で `String` と `Arc` を
        // 確保していたころは、アロケータの時間ぶんだけ全員を待たせていました。
        // すでに組があるとき（同じセッションの 2 本目）は使わずに捨てます。
        let key = id.to_string();
        let fresh: Arc<tokio::sync::Mutex<()>> = Arc::default();
        let (lock_arc, unused) = {
            let mut map = id_locks().lock().unwrap_or_else(|e| e.into_inner());
            match map.get(id) {
                Some(found) => (found.clone(), Some((key, fresh))),
                None => {
                    map.insert(key, fresh.clone());
                    (fresh, None)
                }
            }
        };
        // 使わなかったぶんの解放も、錠の外で。
        drop(unused);
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
        let removed = {
            let mut map = id_locks().lock().unwrap_or_else(|e| e.into_inner());
            // 地図のぶんだけなら、待っている人はいない。
            // 待っている人は地図の錠を取ってから複製するので、ここで数え漏らしません。
            match map.get(&self.id) {
                Some(found) if Arc::strong_count(found) == 1 => map.remove_entry(&self.id),
                _ => None,
            }
        };
        // 外した組（鍵の文字列と `Arc`）の解放は、地図の錠の外で。
        drop(removed);
    }
}

/// リクエストの終わりにすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterRequest {
    /// 何もしない。Cookie も返しません。
    Nothing,
    /// 保存はしないが、Cookie を返して期限を延ばす。
    ///
    /// 置き場所の期限は、読んだときに延ばしてあります（`read_and_touch`）。
    Touch,
    /// 保存して、Cookie を返す。
    Save,
    /// ログアウトした。置き場所から消したので、Cookie も消す。
    ///
    /// [`after_request`] は返しません。保存の経路で消したと分かってから決めます。
    Forget,
}

/// 保存が要るか・置き場所から読めたか・ルートに当たったかで、終わりにすることを決める。
///
/// | 保存が要る | 読めた | ルート | すること |
/// |---|---|---|---|
/// | はい | - | - | 保存して Cookie |
/// | いいえ | はい | あり | Cookie を返す（期限を延ばす） |
/// | いいえ | はい | なし | 何もしない |
/// | いいえ | いいえ | - | 何もしない |
///
/// **「Cookie を持っていた」ではなく「置き場所から読めた」で決めます。**
/// 署名は合うが中身が無い Cookie（期限切れ・ログアウト後）の人に Cookie を返すと、
/// 保存していない新しい ID を毎リクエスト配り続けることになります。
///
/// **ルートに当たらなかったときは、書き換えていなければ何もしません。**
/// 静的ファイルの応答に `Set-Cookie` を付ける意味はありません
/// （「保存が要る」の中身もルートの有無で違います。呼ぶ側を参照）。
fn after_request(saved: bool, loaded: bool, matched: bool) -> AfterRequest {
    match (saved, loaded && matched) {
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
    // 内部の鍵（`__` 始まり）なので、警告を出さない口から入れる。
    session.put_internal(CSRF_KEY, token.clone());
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
        assert_eq!(after_request(false, false, true), AfterRequest::Nothing);
        // 読めたときは、読むだけのリクエストでも期限を延ばす。
        assert_eq!(after_request(false, true, true), AfterRequest::Touch);
        // 保存が要るときは保存する。
        assert_eq!(after_request(true, false, true), AfterRequest::Save);
        assert_eq!(after_request(true, true, true), AfterRequest::Save);
    }

    #[test]
    fn ルートに当たらなければ書き換えたときだけcookieを返す() {
        // 静的ファイル・404・405 の応答に、読めたというだけで `Set-Cookie` を付けない。
        assert_eq!(after_request(false, true, false), AfterRequest::Nothing);
        assert_eq!(after_request(false, false, false), AfterRequest::Nothing);
        // ミドルウェアが書き換えたときは保存する。
        assert_eq!(after_request(true, true, false), AfterRequest::Save);
        assert_eq!(after_request(true, false, false), AfterRequest::Save);
    }

    #[test]
    fn 知らない置き場所の名前はfileにする() {
        // 警告を出してから file にする（黙って file にしていた）。
        let kind = |driver: &str| StartSession::from_driver(driver, SessionConfig::default()).kind;
        assert_eq!(kind("file"), StoreKind::File);
        assert_eq!(kind("memory"), StoreKind::Memory);
        assert_eq!(kind("array"), StoreKind::Memory);
        assert_eq!(kind("redis"), StoreKind::File, "知らない値");
        assert_eq!(kind("flie"), StoreKind::File, "書き間違い");
        assert_eq!(kind(""), StoreKind::File);
    }

    #[test]
    fn 変えた鍵だけを当てる() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        // 読んだとき。
        let base = map(&[("a", "1"), ("b", "2"), ("c", "3")]);
        // こちらは b を変え、c を消し、d を足した。a は触っていない。
        let ours = map(&[("a", "1"), ("b", "9"), ("d", "4")]);
        // そのあいだに、別のリクエストが a を変え、e を足して保存していた。
        let fresh = map(&[("a", "7"), ("b", "2"), ("c", "3"), ("e", "5")]);

        assert_eq!(
            merge_changes(&base, &ours, fresh),
            map(&[("a", "7"), ("b", "9"), ("d", "4"), ("e", "5")]),
            "触っていない鍵は置き場所の値を残す"
        );

        // 何も変えていなければ、置き場所の中身のまま。
        let fresh = map(&[("a", "7")]);
        assert_eq!(merge_changes(&base, &base, fresh.clone()), fresh);
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
        // 「保存した」と分けて返す。呼ぶ側が、新しい ID の Cookie を配らずに
        // 古い Cookie を消すと決められるようにするため。
        assert_eq!(outcome, SaveOutcome::Destroyed);
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

    // ---- ここから下は、読み込みから Cookie までを通しで確かめます ----

    use crate::http::handler::{Erased, ErasedHandler};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TEST_KEY: &[u8] = b"test-key";

    /// 呼ばれた回数を数える置き場所。中身は `MemoryStore` に任せます。
    struct Counting {
        inner: crate::session::MemoryStore,
        reads: AtomicUsize,
        reads_with_touch: AtomicUsize,
        touches: AtomicUsize,
        writes: AtomicUsize,
    }

    impl Counting {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                inner: crate::session::MemoryStore::new(60),
                reads: AtomicUsize::new(0),
                reads_with_touch: AtomicUsize::new(0),
                touches: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
            })
        }

        /// (`read`, `read_and_touch`, `touch`, `write`) の回数。
        fn counts(&self) -> (usize, usize, usize, usize) {
            (
                self.reads.load(Ordering::Relaxed),
                self.reads_with_touch.load(Ordering::Relaxed),
                self.touches.load(Ordering::Relaxed),
                self.writes.load(Ordering::Relaxed),
            )
        }

        /// 数えずに中身を見る（テストの確認用）。
        fn peek(&self, id: &str) -> Option<BTreeMap<String, String>> {
            self.inner.read(id).unwrap()
        }

        /// 数えずに中身を置く（テストの準備用）。
        fn seed(&self, id: &str, pairs: &[(&str, &str)]) {
            self.inner.write(id, &pairs_to_map(pairs)).unwrap();
        }
    }

    impl SessionStore for Counting {
        fn read(&self, id: &str) -> Result<Option<BTreeMap<String, String>>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.inner.read(id)
        }
        fn write(&self, id: &str, data: &BTreeMap<String, String>) -> Result<()> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            self.inner.write(id, data)
        }
        fn destroy(&self, id: &str) -> Result<()> {
            self.inner.destroy(id)
        }
        fn touch(&self, id: &str) -> Result<()> {
            self.touches.fetch_add(1, Ordering::Relaxed);
            self.inner.touch(id)
        }
        fn read_and_touch(&self, id: &str) -> Result<Option<BTreeMap<String, String>>> {
            self.reads_with_touch.fetch_add(1, Ordering::Relaxed);
            self.inner.read_and_touch(id)
        }
        fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
            self.inner.destroy_for_user(user_id)
        }
        fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
            self.inner.destroy_for_user_except(user_id, keep_id)
        }
        fn mark_revoked(&self, user_id: &str, except: Option<&str>) {
            self.inner.mark_revoked(user_id, except);
        }
        fn revoked_at(&self, user_id: &str, session_id: &str) -> Option<i64> {
            self.inner.revoked_at(user_id, session_id)
        }
    }

    fn pairs_to_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// ハンドラを 1 つ包む。
    fn handler<F, Fut>(f: F) -> Arc<dyn ErasedHandler>
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Response>> + Send + 'static,
    {
        Arc::new(Erased::<F, (Request,)>::new(f))
    }

    /// 何もしないハンドラ。
    fn idle() -> Arc<dyn ErasedHandler> {
        handler(|_req: Request| async { Ok(Response::text("ok")) })
    }

    /// 1 リクエストを `StartSession` の本体に通す。
    ///
    /// `cookie` はセッションの Cookie の値です（前の応答から取り出したもの）。
    /// `matched` はルートに当たったことにするかどうかです。
    async fn send(
        store: &Arc<Counting>,
        cookie: Option<&str>,
        matched: bool,
        handler: Arc<dyn ErasedHandler>,
    ) -> Response {
        let mut req = Request::new("GET", "/");
        if let Some(cookie) = cookie {
            req = req.with_headers(vec![(
                "cookie".to_string(),
                format!("bengara_session={cookie}"),
            )]);
        }
        if matched {
            req.mark_matched();
        }
        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let next = Next::new(
            stack,
            handler,
            crate::http::response::RenderOptions::new(false, false),
        );
        serve(store.clone(), SessionConfig::default(), TEST_KEY, req, next)
            .await
            .expect("応答が返る")
    }

    /// 応答の `Set-Cookie` から、セッションの Cookie の値を取り出す。
    fn cookie_of(response: &Response) -> Option<String> {
        let header = response.header("set-cookie")?;
        let (_, value) = header.split(';').next()?.split_once('=')?;
        Some(value.to_string())
    }

    /// ログイン中の利用者 ID を本文で返すハンドラ（いなければ空）。
    fn whoami() -> Arc<dyn ErasedHandler> {
        handler(
            |req: Request| async move { Ok(Response::text(req.auth().id().unwrap_or_default())) },
        )
    }

    #[tokio::test]
    async fn 他の端末を切ってidを作り直しても次の保存で落ちない() {
        // `logout_other_devices()` の印は「切った時刻・除外は切ったときの ID」。
        // そのあと `regenerate()` すると新しい ID は除外に当たらず、ログインの時刻も
        // 古いままなので、**次のリクエストの保存**で本人が切られていた
        // （切ったリクエストの保存は、作り直す前の ID が除外に当たって助かる）。
        let store = Counting::new();

        // 1. ログインする。
        let res = send(
            &store,
            None,
            true,
            handler(|req: Request| async move {
                req.auth().login_using_id(7)?;
                Ok(Response::text("ok"))
            }),
        )
        .await;
        let first = cookie_of(&res).expect("ログインで Cookie が出る");

        // 別の端末のセッション。
        store.seed(
            "other",
            &[
                (crate::auth::AUTH_ID_KEY, "7"),
                (crate::auth::AUTH_AT_KEY, "1000"),
            ],
        );
        // 印の時刻がログインの時刻より確実に後になるようにする。
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        // 2. 他の端末を切って、ID を作り直す（パスワード変更の流れ）。
        let res = send(
            &store,
            Some(&first),
            true,
            handler(|req: Request| async move {
                let removed = req.auth().logout_other_devices_async().await?;
                req.session().regenerate();
                Ok(Response::text(removed.to_string()))
            }),
        )
        .await;
        assert_eq!(res.body_text(), "1", "他の端末は切れる");
        assert!(store.peek("other").is_none());
        let second = cookie_of(&res).expect("作り直した ID の Cookie");
        assert_ne!(second, first);

        // 3. 次のリクエストが保存する（CSRF トークンを作り直す GET に当たる）。
        let res = send(
            &store,
            Some(&second),
            true,
            handler(|req: Request| async move {
                req.session().put("visited", "1");
                Ok(Response::text(req.auth().id().unwrap_or_default()))
            }),
        )
        .await;
        assert_eq!(res.body_text(), "7");
        assert!(
            cookie_of(&res).is_some(),
            "保存できている（切られていない）"
        );

        // 4. その次も、本人はログインしたまま。
        let res = send(&store, Some(&second), true, whoami()).await;
        assert_eq!(res.body_text(), "7", "呼んだ本人は残る");

        // 5. もう一度作り直しても残る。
        let res = send(
            &store,
            Some(&second),
            true,
            handler(|req: Request| async move {
                req.session().regenerate();
                Ok(Response::text("ok"))
            }),
        )
        .await;
        let third = cookie_of(&res).expect("作り直した ID の Cookie");
        let res = send(&store, Some(&third), true, whoami()).await;
        assert_eq!(res.body_text(), "7", "ID を何度作り直しても残る");
    }

    #[tokio::test]
    async fn 読むだけのgetは置き場所を一度しか読まない() {
        // 終わりに `touch` を別に呼んでいたころは、期限を知るためだけに
        // 同じ中身をもう 1 回読んでいた（裏のスレッドへの往復も 2 回）。
        let store = Counting::new();
        store.seed("s1", &[("cart", "1")]);
        let cookie = sign("s1", TEST_KEY);

        let res = send(&store, Some(&cookie), true, idle()).await;
        assert_eq!(
            store.counts(),
            (0, 1, 0, 0),
            "読みと期限の延長を 1 回で済ませる"
        );
        // Cookie は返す（ブラウザ側の期限を延ばす）。ID は変わらない。
        assert_eq!(cookie_of(&res).as_deref(), Some(cookie.as_str()));
    }

    #[tokio::test]
    async fn ルートに当たらないリクエストは読むだけ() {
        // 静的ファイル・404・405。持ち越しの flash を消費せず、書かず、
        // 期限も延ばさず、Cookie も返さない。
        let store = Counting::new();
        store.seed("s1", &[("cart", "1"), ("__flash:status", "保存しました")]);
        let cookie = sign("s1", TEST_KEY);
        let status = || {
            handler(
                |req: Request| async move { Ok(Response::text(req.session().get_or("status", ""))) },
            )
        };

        // セッションは載っている（`req.session()` がパニックしない）。flash も読める。
        let res = send(&store, Some(&cookie), false, status()).await;
        assert_eq!(res.body_text(), "保存しました");
        assert!(res.header("set-cookie").is_none(), "Cookie を返さない");
        assert_eq!(store.counts(), (1, 0, 0, 0), "読むだけ。延ばさず、書かない");
        assert_eq!(
            store
                .peek("s1")
                .unwrap()
                .get("__flash:status")
                .map(String::as_str),
            Some("保存しました"),
            "flash は消費していない"
        );

        // 次の、ルートに当たるリクエストで読めて、そこで消費する。
        let res = send(&store, Some(&cookie), true, status()).await;
        assert_eq!(res.body_text(), "保存しました", "画面で 1 回読める");
        let res = send(&store, Some(&cookie), true, status()).await;
        assert_eq!(res.body_text(), "", "その次には残らない");

        // Cookie を持たない人には、ルートが無ければ何も起きない。
        let before = store.counts();
        let res = send(&store, None, false, idle()).await;
        assert!(res.header("set-cookie").is_none());
        assert_eq!(store.counts(), before, "置き場所に触らない");
    }

    #[tokio::test]
    async fn ルートに当たらないリクエストは錠を待たない() {
        // 同じブラウザの画面 1 枚と CSS・画像が 1 本ずつにならないこと。
        let store = Counting::new();
        store.seed("locked", &[("cart", "1")]);
        let cookie = sign("locked", TEST_KEY);

        // 同じセッションの、長くかかるリクエストが錠を持っている。
        let held = IdLock::acquire("locked").await;

        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            send(&store, Some(&cookie), false, idle()),
        )
        .await
        .expect("錠を待たずに返る");
        assert_eq!(res.status(), 200);

        // ルートに当たるリクエストは、今までどおり待つ。
        let waiting = {
            let store = store.clone();
            let cookie = cookie.clone();
            tokio::spawn(async move { send(&store, Some(&cookie), true, idle()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "放すまで待つ");
        drop(held);
        waiting.await.expect("待っていたほうが返る");
    }

    #[tokio::test]
    async fn ルートに当たらないリクエストの書き換えは変えた鍵だけ保存する() {
        // 錠を持たずに読んでいるので、丸ごと書き戻すと、そのあいだに保存された
        // 別のリクエストの値（作ったばかりの CSRF トークンなど）を消してしまう。
        let store = Counting::new();
        store.seed(
            "s1",
            &[("a", "1"), ("b", "2"), ("__flash:status", "保存しました")],
        );
        let cookie = sign("s1", TEST_KEY);

        let res = {
            let shared = store.clone();
            send(
                &store,
                Some(&cookie),
                false,
                handler(move |req: Request| {
                    let shared = shared.clone();
                    async move {
                        // 読んだあとで、別のリクエストがトークンを足して保存した。
                        shared.seed(
                            "s1",
                            &[
                                ("a", "1"),
                                ("b", "2"),
                                (CSRF_KEY, "別のリクエストのトークン"),
                                ("__flash:status", "保存しました"),
                            ],
                        );
                        // こちらは b を変え、a を消す。
                        req.session().put("b", "9");
                        req.session().forget("a");
                        Ok(Response::text("ok"))
                    }
                }),
            )
            .await
        };
        assert!(cookie_of(&res).is_some(), "書き換えたので Cookie を返す");
        assert_eq!(
            store.peek("s1").unwrap(),
            pairs_to_map(&[
                ("b", "9"),
                (CSRF_KEY, "別のリクエストのトークン"),
                ("__flash:status", "保存しました"),
            ]),
            "変えた鍵だけ当てる。別のリクエストの値と flash は残す"
        );
        // 使い終わった錠は地図に残さない。
        assert!(!id_locks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key("s1"));

        // 処理中にセッションが消えていたら、書き戻さない（ログアウトを取り消さない）。
        let res = {
            let shared = store.clone();
            send(
                &store,
                Some(&cookie),
                false,
                handler(move |req: Request| {
                    let shared = shared.clone();
                    async move {
                        shared.destroy("s1")?;
                        req.session().put("b", "10");
                        Ok(Response::text("ok"))
                    }
                }),
            )
            .await
        };
        assert!(res.header("set-cookie").is_none());
        assert!(store.peek("s1").is_none(), "消えたセッションを復活させない");

        // 新しいセッション（Cookie なし）でも、書き換えれば保存して Cookie を返す。
        let res = send(
            &store,
            None,
            false,
            handler(|req: Request| async move {
                req.session().put("locale", "ja");
                Ok(Response::text("ok"))
            }),
        )
        .await;
        let fresh = cookie_of(&res).expect("新しいセッションの Cookie");
        let res = send(
            &store,
            Some(&fresh),
            true,
            handler(|req: Request| async move {
                Ok(Response::text(req.session().get_or("locale", "")))
            }),
        )
        .await;
        assert_eq!(res.body_text(), "ja");
    }

    #[tokio::test]
    async fn ログアウトしたら新しいidを配らずcookieを消す() {
        // 以前は、置き場所から消したあとも保存していない新しい ID の Cookie を返していた。
        let store = Counting::new();
        let logout = || {
            handler(|req: Request| async move {
                req.auth().logout()?;
                Ok(Response::text("ok"))
            })
        };

        let res = send(
            &store,
            None,
            true,
            handler(|req: Request| async move {
                req.auth().login_using_id(7)?;
                Ok(Response::text("ok"))
            }),
        )
        .await;
        let cookie = cookie_of(&res).expect("ログインで Cookie が出る");

        let res = send(&store, Some(&cookie), true, logout()).await;
        let header = res.header("set-cookie").expect("消す Cookie を返す");
        assert!(header.starts_with("bengara_session=;"), "{header}");
        assert!(header.contains("Max-Age=0"), "{header}");
        assert!(header.contains("Path=/"), "{header}");
        assert!(header.contains("HttpOnly"), "{header}");

        // 古い Cookie を送り直しても、ログインしていない。Cookie も返らない。
        let res = send(&store, Some(&cookie), true, whoami()).await;
        assert_eq!(res.body_text(), "");
        assert!(res.header("set-cookie").is_none());

        // Cookie を持っていない人のログアウトには、何も返さない。
        let res = send(&store, None, true, logout()).await;
        assert!(res.header("set-cookie").is_none());

        // ログアウトのあとで値を入れたとき（「ログアウトしました」の flash など）は、
        // 新しい ID で保存して、その Cookie を返す。
        let res = send(
            &store,
            None,
            true,
            handler(|req: Request| async move {
                req.auth().login_using_id(7)?;
                Ok(Response::text("ok"))
            }),
        )
        .await;
        let cookie = cookie_of(&res).unwrap();
        let res = send(
            &store,
            Some(&cookie),
            true,
            handler(|req: Request| async move {
                req.auth().logout()?;
                req.session().flash("status", "ログアウトしました");
                Ok(Response::text("ok"))
            }),
        )
        .await;
        let after = cookie_of(&res).expect("新しい ID の Cookie");
        assert_ne!(after, cookie);
        assert!(!after.is_empty());
        let res = send(
            &store,
            Some(&after),
            true,
            handler(|req: Request| async move {
                Ok(Response::text(format!(
                    "{}/{}",
                    req.auth().id().unwrap_or_default(),
                    req.session().get_or("status", "")
                )))
            }),
        )
        .await;
        assert_eq!(res.body_text(), "/ログアウトしました");
    }

    #[tokio::test]
    async fn 錠の地図は確保を外に出しても片付く() {
        // 組を先に作ってから地図に入れる形にしても、数え方（地図のぶんだけなら
        // 待っている人はいない）が崩れていないこと。
        let id = format!("test-{}", crypto::random_token());
        let in_map = |id: &str| {
            id_locks()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(id)
        };

        let first = IdLock::acquire(&id).await;
        assert!(in_map(&id), "持っているあいだは地図にある");
        // 2 本目は、先に作った組を捨てて、地図の組を使う。
        let waiting = {
            let id = id.clone();
            tokio::spawn(async move { IdLock::acquire(&id).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "同じ組を待っている");

        drop(first);
        assert!(in_map(&id), "待っている人がいるあいだは片付けない");
        let second = waiting.await.expect("待っていたほうが取れる");
        drop(second);
        assert!(!in_map(&id), "最後の 1 人が放したら片付ける");
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
