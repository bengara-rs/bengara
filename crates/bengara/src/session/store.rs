//! セッションの置き場所。
//!
//! 既定は `FileStore`（`storage/framework/sessions/`）です。
//! テストや、ディスクを使いたくないときは `MemoryStore` を使えます。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::support::crypto;
use crate::support::fs::{write_atomic, Access};
use crate::support::time::now_seconds_u64;
use crate::support::MEMORY_GC_EVERY;

type Data = BTreeMap<String, String>;

/// セッションの中身を置いておく場所。
///
/// 自分で作るときは、次の3つを満たしてください。
///
/// - **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
/// - 期限切れは読み込みのときに捨ててください。
/// - 読み書きはブロックしてかまいません（`spawn_blocking` の中で呼びます）。
pub trait SessionStore: Send + Sync + 'static {
    /// ID から中身を読む。無ければ `None`。
    fn read(&self, id: &str) -> Result<Option<Data>>;

    /// 中身を書く。
    fn write(&self, id: &str, data: &Data) -> Result<()>;

    /// 中身を消す。
    fn destroy(&self, id: &str) -> Result<()>;

    /// 中身を変えずに**期限だけ延ばす**。
    ///
    /// 読むだけのリクエスト（GET の画面）でも呼ばれます。延ばさないと、
    /// 置き場所の期限が「最後に書いたとき」から数えられるので、
    /// 画面を見て回っていた人が突然ログアウトされます。
    ///
    /// **既定は `read` して、同じ中身で `write` し直すだけです。**
    /// 置き場所が期限を知っているなら、最後に書いてから少し（組み込みの置き場所は
    /// `min(60 秒, 期限の 1/10)`）過ぎたときだけ書く形にしてください
    /// （`FileStore` と `MemoryStore` はそうしています）。間引きすぎると、
    /// 放っておいて切れるまでの時間が期限より短くなります。
    /// 置き場所に無い ID のときは何もしません。
    ///
    /// **期限切れを延命してはいけません。** 残りが 0 秒のものは「書いてから時間が
    /// 過ぎた」にも当たるので、期限を確かめずに書き直すと、切れたセッションに満額の寿命を
    /// 与え直すことになります（`FileStore` が実際にそうなっていました）。
    /// 既定の実装は `read` が期限切れを捨てるので、ここは守れています。
    fn touch(&self, id: &str) -> Result<()> {
        if let Some(data) = self.read(id)? {
            self.write(id, &data)?;
        }
        Ok(())
    }

    /// 中身を読み、**ついでに期限も延ばす**（[`read`](Self::read) と
    /// [`touch`](Self::touch) を 1 回で済ませる）。
    ///
    /// `StartSession` が、ルートに当たったリクエストの最初に呼びます。
    /// 別々に呼ぶと、期限を知るためだけに同じ中身を 2 回読むことになります
    /// （`FileStore` はファイルを 2 回読んで解析していました）。
    ///
    /// **既定は `read` のあとに `touch` を呼ぶだけです。** 動きは別々に呼ぶのと
    /// 同じなので、自分で作った置き場所はそのままで壊れません。1 回の読みで
    /// 期限まで分かる置き場所では、自分で実装してください
    /// （`FileStore` と `MemoryStore` はそうしています）。
    ///
    /// 期限を延ばせなかったときは**警告だけ出して、読めた中身を返します。**
    /// 延ばせないだけでリクエストを落とすほどのことではありません。
    fn read_and_touch(&self, id: &str) -> Result<Option<Data>> {
        let found = self.read(id)?;
        if found.is_some() {
            if let Err(e) = self.touch(id) {
                tracing::warn!("セッションの期限を延ばせませんでした: {e}");
            }
        }
        Ok(found)
    }

    /// ある利用者のセッションを**全部**消す。消した数を返す。
    ///
    /// 利用者 ID は、セッションの中の `bengara_auth_id`（`Auth::login` が入れる値）です。
    /// `Auth::logout_all_devices()` から呼ばれます。
    ///
    /// **既定では何もせず 0 を返します。** 自分で作った置き場所が
    /// 急に壊れないようにするためです。利用者で引く方法は置き場所ごとに違うので、
    /// 必要になったときに実装してください。
    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        let _ = user_id;
        warn_not_implemented("destroy_for_user");
        Ok(0)
    }

    /// ある利用者のセッションのうち、`keep_id` **以外**を消す。消した数を返す。
    ///
    /// `Auth::logout_other_devices()` から呼ばれます。いま操作している人を
    /// ログアウトさせずに、ほかの端末だけ切るための形です。
    ///
    /// 既定では何もせず 0 を返します（`destroy_for_user` と同じ理由）。
    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        let _ = (user_id, keep_id);
        warn_not_implemented("destroy_for_user_except");
        Ok(0)
    }

    /// 「この利用者のセッションを切った」印を付ける。
    ///
    /// `destroy_for_user` / `destroy_for_user_except` はファイルを消すだけなので、
    /// **その瞬間に処理中だった別の端末のリクエスト**が、保存の経路で同じ ID を
    /// 書き直すと、切ったはずのセッションが復活します。印を残しておけば、
    /// `StartSession` が保存の直前に「ログインした時刻より新しい印がある」と気づいて
    /// 保存せずに捨てられます。
    ///
    /// `except` は残したセッションの ID（`destroy_for_user_except` の `keep_id`）です。
    /// その ID だけは印に当たりません。
    ///
    /// 既定では何もしません。自分で作った置き場所を壊さないためです。
    /// その場合、上の取りこぼしは塞がれません。
    fn mark_revoked(&self, user_id: &str, except: Option<&str>) {
        let _ = (user_id, except);
    }

    /// `session_id` に効く「切った」印の時刻（UNIX ミリ秒）。無ければ `None`。
    ///
    /// ミリ秒なのは、切った直後に同じ利用者がログインし直したセッションを
    /// 巻き込まないためです（ログインの時刻もミリ秒で持ちます）。
    ///
    /// `mark_revoked` で `except` にした ID には `None` を返します。
    ///
    /// **古い印は返さないでください**（組み込みの置き場所は 1 時間）。
    /// 印が要るのは、切った瞬間に処理中だったリクエストだけです。いつまでも
    /// 返すと、ずっと後のリクエストまで「切られた」と判定されます。
    fn revoked_at(&self, user_id: &str, session_id: &str) -> Option<i64> {
        let _ = (user_id, session_id);
        None
    }
}

/// 「切った」印が効く秒数。
///
/// 印が要るのは、切った瞬間に処理中だったリクエストだけです。
/// リクエストが 1 時間を超えて走ることはないので、それより古い印は
/// **読むときに無視します**（[`mark_is_fresh`]）。ファイルそのものは掃除が消します。
///
/// 読むときに見ないと、掃除が走るまで（`MemoryStore` では次に誰かを切るまで）
/// 印が効き続けます。
const REVOKED_MARK_SECS: u64 = 3600;

/// 「切った」印がまだ効くか。`at` は印の時刻（UNIX ミリ秒）です。
///
/// 時計が戻って印のほうが未来になったときは、効くほうに倒します。
fn mark_is_fresh(at: i64) -> bool {
    let limit = i64::try_from(REVOKED_MARK_SECS * 1000).unwrap_or(i64::MAX);
    crate::support::time::now_millis().saturating_sub(at) <= limit
}

/// 中身に入っているログインの時刻より後に、その利用者が切られているか。
///
/// 期限を延ばす書き込みの直前に見ます。`StartSession` が保存の直前に見るのと
/// 同じ判定です（ログインしていない中身は対象外。時刻が無ければ 0 として扱う）。
fn revoked_since_login(store: &dyn SessionStore, id: &str, data: &Data) -> bool {
    let Some(user) = data.get(crate::auth::AUTH_ID_KEY) else {
        return false;
    };
    let logged_in_at = data
        .get(crate::auth::AUTH_AT_KEY)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    store
        .revoked_at(user, id)
        .is_some_and(|at| at > logged_in_at)
}

/// 一時ファイル（`<ID>.<乱数>.tmp`）がこの秒数より古ければ、書き込みに失敗した
/// 残骸と見なして消す。`FileCache` と同じ数字です。
const TEMP_STALE_SECS: u64 = 300;

/// 書き込みの何回に 1 回、期限切れを掃除するか（Laravel の lottery と同じ考え方）。
///
/// Cookie を持たない人がルートに当たるたびに新しいセッションが 1 つできます。
/// その人が二度と来なければ `read` で消える機会も無いので、書き込みのついでに
/// 少しずつ掃除します。
///
/// 抽選は 1 バイトの乱数（0..=255）で引きます。`(2, 100)` は
/// 「`draw * 100 < 2 * 256`」＝ `draw` が 0..=5 のときに当たるので、
/// 実際の割合は **6/256（約 2.3%）** です。切り上げのぶん 2% より少し多めです。
const SWEEP_LOTTERY: (u8, u8) = (2, 100);

/// 書き込みのついでの掃除が、1 回で見るファイルの数の上限。
///
/// 上限が無いと、抽選に当たった 1 回が置き場所の全ファイルを解析します。
/// セッションが数万件ある所では掃除のスレッドが長時間ふさがるので、ここで
/// 打ち切って最悪の時間を決めます。
///
/// 見る範囲は**毎回でたらめな位置から**始めます（末尾まで来たら先頭に戻る）。
/// 先頭から数えていたころは、`read_dir` の順が毎回ほぼ同じなので、
/// 501 件目より後ろのファイルが永久に掃除されませんでした。
/// 名前の一覧は毎回全部取りますが、開いて解析するのは上限の数だけです。
///
/// `session:gc` から呼ぶ経路は**上限なし**で全部見ます（コマンドは待ってよい）。
const SWEEP_MAX_ENTRIES: usize = 500;

/// 書き込みのついでの掃除がいま走っているか。
///
/// 掃除は別のスレッドに投げっぱなしにします。走っているあいだに抽選に当たって
/// また投げると、同じディレクトリを何本ものスレッドが走査します。
static SWEEPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 「切った」印を置くディレクトリ。`is_session_id` が通さない `+` で始めます。
const REVOKED_DIR: &str = "+revoked";

/// 既定のままの実装を呼ばれたことを知らせる。
///
/// 黙って 0 を返すと、「他の端末を切ったつもりで切れていない」ことに気づけません。
fn warn_not_implemented(method: &str) {
    tracing::warn!(
        "この SessionStore は {method} を実装していないため、\
         ほかの端末のセッションは切れていません"
    );
}

/// セッション ID として通す形か。
///
/// ID は自分で作った16進の文字列ですが、外から来た値でもあるので念のため確かめます。
/// `..` や `/` が混じったファイル名を作らせないためです。
///
/// 通すのは**英数字とハイフンだけ**です。ここで**ドットを通さない**ことが、
/// `write` の一時ファイル（`<ID>.<乱数>.tmp`）がセッションのファイル名と
/// ぶつからない根拠になっています。この条件を緩めるときは `write` も見直してください。
///
/// `destroy_matching` と `sweep` も、ディレクトリの中のファイルを触るかどうかを
/// **この判定で**決めます。書きかけの一時ファイルを消すと `rename` が失敗します。
fn is_session_id(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `storage/framework/sessions/` にファイルとして置く。既定の置き場所です。
///
/// 1つのセッションが1つのファイルになります。
/// プロセスを2つ以上動かすときは、全プロセスで同じ場所を指してください。
pub struct FileStore {
    dir: PathBuf,
    lifetime_secs: u64,
    /// ディレクトリの権限を直したか（`ensure_dir` が1回だけ呼びます）。
    restricted: std::sync::Once,
}

impl FileStore {
    /// 置き場所と、放っておいて消えるまでの秒数を決めて作る。
    pub fn new(dir: impl Into<PathBuf>, lifetime_secs: u64) -> Self {
        Self {
            dir: dir.into(),
            lifetime_secs,
            restricted: std::sync::Once::new(),
        }
    }

    /// `storage/framework/sessions/` に置く（既定）。
    pub fn default_path(lifetime_secs: u64) -> Self {
        Self::new(
            crate::paths::storage_path("framework/sessions"),
            lifetime_secs,
        )
    }

    /// ID からファイルの場所を決める。
    fn path_of(&self, id: &str) -> Result<PathBuf> {
        if !is_session_id(id) {
            return Err(Error::msg("セッション ID の形が正しくありません"));
        }
        Ok(self.dir.join(id))
    }

    fn ensure_dir(&self) -> Result<()> {
        if self.dir.is_dir() {
            // 既にあるときも権限を直す。tar の展開などで 0755 のまま置かれると、
            // ファイルが 0600 でも**ディレクトリが読めるので、
            // ファイル名＝セッション ID を一覧できてしまいます。**
            // 書き込みごとに直す意味は無いので、1回だけにします。
            self.restricted.call_once(|| restrict_dir(&self.dir));
            return Ok(());
        }
        // storage/ そのものが無いなら、作らずに知らせる（決定記録 #023）。
        let storage = crate::paths::storage_path("");
        if !storage.is_dir() {
            return Err(Error::msg(crate::paths::missing_storage_message(&storage)));
        }
        std::fs::create_dir_all(&self.dir)?;
        self.restricted.call_once(|| restrict_dir(&self.dir));
        Ok(())
    }

    /// 利用者 ID が一致するセッションのファイルを消す。`keep` に渡した ID は残す。
    ///
    /// **ディレクトリの中を全部読んで中身を見ます。** 索引は持ちません。
    /// 呼ぶのはパスワードの変更やログアウトのときだけで、回数が少ないからです。
    /// 索引（利用者 ID → セッション ID のファイル）を作ると、
    /// 保存のたびに2つのファイルを書くことになり、整合も自分で守らなければなりません。
    fn destroy_matching(&self, user_id: &str, keep: Option<&str>) -> Result<usize> {
        // 消す**前に**印を付ける。後に付けると、消したあと印を書く前に保存されたものが
        // 復活しても気づけません。
        self.mark_revoked(user_id, keep);

        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(Error::Io(e)),
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // 書きかけの一時ファイルなどは触らない（消すと rename が失敗する）。
            if !is_session_id(&name) {
                continue;
            }
            if keep == Some(name.as_str()) {
                continue;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let found = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok());
            // 読めないファイルはここでは触らない（`sweep` の仕事）。
            let Some(stored) = found else { continue };
            if stored
                .data
                .get(crate::auth::AUTH_ID_KEY)
                .map(String::as_str)
                != Some(user_id)
            {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// 「切った」印のファイル名。
    ///
    /// 利用者 ID はアプリが決める文字列なので、そのままファイル名にせず
    /// SHA-256 の 16 進にします。**ドットが入らない**ので、`write` と同じく
    /// `<名前>.<乱数>.tmp` の一時ファイルとぶつかりません。
    fn revoked_name(user_id: &str) -> String {
        crypto::to_hex(&crypto::sha256(user_id.as_bytes()))
    }

    /// 「切った」印のファイルの場所。
    fn revoked_path_of(&self, user_id: &str) -> PathBuf {
        self.dir.join(REVOKED_DIR).join(Self::revoked_name(user_id))
    }

    /// 書き込みの何回かに 1 回、期限切れを掃除する。
    ///
    /// **掃除は別のスレッドに投げます。** 応答の経路でやると、抽選に当たった
    /// リクエストが置き場所を走査し終わるまで返りません。`StartSession` の
    /// ID ごとの錠を握ったままなので、その利用者の次のリクエストも全部待ちます。
    ///
    /// 投げっぱなしにして、`write` の返り値には影響させません
    /// （掃除に失敗しても保存は成功させます。次の機会にまた当たります）。
    /// 走っているあいだは、抽選に当たっても始めません。
    fn maybe_sweep(&self) {
        use std::sync::atomic::Ordering;

        let draw = crypto::random_bytes::<1>()[0];
        // 0..=255 のうち、当たる幅（`SWEEP_LOTTERY` を参照）。
        let (hits, out_of) = SWEEP_LOTTERY;
        if u32::from(draw) * u32::from(out_of) >= u32::from(hits) * 256 {
            return;
        }
        // すでに走っていれば何もしない。
        if SWEEPING.swap(true, Ordering::AcqRel) {
            return;
        }
        let dir = self.dir.clone();
        let spawned = std::thread::Builder::new()
            .name("bengara-session-sweep".to_string())
            .spawn(move || {
                if let Err(e) = sweep_limited(&dir, Some(SWEEP_MAX_ENTRIES)) {
                    tracing::warn!("セッションの掃除に失敗しました: {e}");
                }
                SWEEPING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            // スレッドを作れなかったときは目印を戻す。
            // 戻さないと、このプロセスでは二度と掃除が始まりません。
            SWEEPING.store(false, Ordering::Release);
            tracing::warn!("セッションの掃除のスレッドを作れませんでした");
        }
    }
}

impl FileStore {
    /// ファイルを読んで解析する。無い・壊れている・期限切れは `None`
    /// （壊れたものと期限切れは、その場で消します）。
    ///
    /// `read` と `read_and_touch` の共通部分です。**期限も一緒に返す**ので、
    /// 期限を知るために読み直す必要がありません。
    fn load(&self, id: &str) -> Result<Option<Stored>> {
        let path = self.path_of(id)?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Io(e)),
        };
        let stored: Stored = match serde_json::from_str(&raw) {
            Ok(stored) => stored,
            // 壊れていたら無かったことにする。落とすほどのことではない。
            Err(_) => {
                tracing::warn!("セッションのファイルを読めませんでした: {}", path.display());
                let _ = std::fs::remove_file(&path);
                return Ok(None);
            }
        };
        if stored.expires_at <= now_seconds_u64() {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(stored))
    }

    /// 読めた中身の期限を、最後に書いてから少し過ぎていれば延ばす（[`needs_touch`]）。
    ///
    /// **期限切れでないことは呼ぶ側が確かめておきます。**
    ///
    /// 書く直前に「切った」印を見ます。ここは「読む → 書く」のあいだに
    /// `logout_user` などがファイルを消すと、消したはずのセッションを
    /// 書き戻してしまう場所です。印は消すより**前に**付くので、印があれば
    /// 書かずに帰ります（ファイルは切る側が消します）。窓は「印を見てから
    /// 置き換えるまで」に縮みます。無くなりはしません。
    fn extend(&self, id: &str, stored: &Stored) -> Result<()> {
        if !needs_touch(stored.expires_at, self.lifetime_secs) {
            return Ok(());
        }
        if revoked_since_login(self, id, &stored.data) {
            return Ok(());
        }
        self.write(id, &stored.data)
    }
}

impl SessionStore for FileStore {
    fn read(&self, id: &str) -> Result<Option<Data>> {
        Ok(self.load(id)?.map(|stored| stored.data))
    }

    /// ファイルを **1 回だけ**読んで、中身を返し、期限も延ばす。
    fn read_and_touch(&self, id: &str) -> Result<Option<Data>> {
        let Some(stored) = self.load(id)? else {
            return Ok(None);
        };
        // 延ばせなくても、読めた中身は返す（致命的ではない）。
        if let Err(e) = self.extend(id, &stored) {
            tracing::warn!("セッションの期限を延ばせませんでした: {e}");
        }
        Ok(Some(stored.data))
    }

    fn write(&self, id: &str, data: &Data) -> Result<()> {
        let path = self.path_of(id)?;
        self.ensure_dir()?;
        let stored = Stored {
            // `SESSION_LIFETIME` に巨大な値を入れてもあふれない。
            expires_at: now_seconds_u64().saturating_add(self.lifetime_secs),
            data: data.clone(),
        };
        let body = serde_json::to_string(&stored)?;

        // 書いている途中のファイルを読まれないように、別名で書いてから置き換える。
        // 一時ファイルも本体と同じ権限で作る（途中だけ読めるのでは意味がない）。
        //
        // 名前に**乱数を混ぜます。** ID だけで決めると、同じ Cookie の2本が同時に
        // 書き込んだときに同じ一時ファイルを `truncate` で開き、混ざった JSON が
        // 本体に置き換わります。次の `read` が失敗してファイルを消すので、
        // 利用者は突然ログアウトされ、次の POST が 419 になります。
        //
        // `is_session_id` がドットを通さないので、この名前は本体とぶつからず、
        // `destroy_matching` と `sweep` も触りません。
        //
        // 権限・Windows のやり直し・失敗時の片付けは `write_atomic` が持ちます。
        // 片付けられなかった一時ファイルは `sweep` が 5 分後に消します。
        let temp = self
            .dir
            .join(format!("{id}.{}.tmp", crypto::random_token()));
        write_atomic(&temp, &path, body.as_bytes(), Access::Private)?;
        self.maybe_sweep();
        Ok(())
    }

    fn destroy(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn touch(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        let Ok(stored) = serde_json::from_str::<Stored>(&raw) else {
            // 壊れたファイルは `read` と `sweep` の仕事。ここでは触らない。
            return Ok(());
        };
        // 期限切れは延命しない。`needs_touch` だけを見ていたころは、
        // 残り 0 秒が「延ばす時期」に当たり、満額の寿命を与え直していた。
        // `MemoryStore::touch` には最初からこの確認がある。
        if stored.expires_at <= now_seconds_u64() {
            return Ok(());
        }
        self.extend(id, &stored)
    }

    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, None)
    }

    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, Some(keep_id))
    }

    /// `+revoked/<sha256(利用者 ID)>` に「ミリ秒 改行 除外した ID」を書く。
    ///
    /// 1 利用者 1 ファイルです。続けて切れば新しい時刻で置き換わります。
    /// 書けなくても `destroy_*` は続けます（印は保険なので）。
    ///
    /// **`write` と同じく、別名に書いてから `rename` で置き換えます。**
    /// 直に書くと、`truncate` で開いてから書き終えるまで中身が 0 バイトの
    /// 瞬間ができます。その瞬間に `revoked_at` が読むと「切られていない」と
    /// 答え、切ったはずのセッションが保存されてしまいます。
    fn mark_revoked(&self, user_id: &str, except: Option<&str>) {
        // `storage/` が無ければ作らずに知らせる（決定記録 #023）。
        // ここで `create_dir_all` を直に呼ぶと、その決めごとを素通りします。
        if let Err(e) = self.ensure_dir() {
            tracing::warn!("切った印の置き場所を作れませんでした: {e}");
            return;
        }
        let dir = self.dir.join(REVOKED_DIR);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("切った印の置き場所を作れませんでした: {e}");
            return;
        }
        let body = format!(
            "{}\n{}",
            crate::support::time::now_millis(),
            except.unwrap_or("")
        );
        let name = Self::revoked_name(user_id);
        let temp = dir.join(format!("{name}.{}.tmp", crypto::random_token()));
        // 失敗したときの一時ファイルは `write_atomic` が片付けます。
        if let Err(e) = write_atomic(&temp, &dir.join(&name), body.as_bytes(), Access::Private) {
            tracing::warn!("セッションを切った印を書けませんでした: {e}");
        }
    }

    fn revoked_at(&self, user_id: &str, session_id: &str) -> Option<i64> {
        let raw = std::fs::read_to_string(self.revoked_path_of(user_id)).ok()?;
        let mut lines = raw.lines();
        let at = lines.next()?.trim().parse::<i64>().ok()?;
        let except = lines.next().unwrap_or("").trim();
        if !except.is_empty() && except == session_id {
            return None;
        }
        // 古い印は無視する。ファイルは掃除が消すまで残るので、ここで見ます。
        mark_is_fresh(at).then_some(at)
    }
}

/// 期限を延ばす書き込みを間引く秒数の上限。
///
/// 最後に書いてからこの秒数（期限が短いときは期限の 1/10）が過ぎていれば延ばします。
const TOUCH_SLACK_MAX_SECS: u64 = 60;

/// 延ばす書き込みを間引く秒数。`min(60 秒, 期限の 1/10)` です。
fn touch_slack(lifetime_secs: u64) -> u64 {
    TOUCH_SLACK_MAX_SECS.min(lifetime_secs / 10)
}

/// 期限を延ばし直すか。**最後に書いてから [`touch_slack`] が過ぎたときだけ**書きます。
///
/// 毎リクエスト書くと、GET の画面を開くたびにファイルを置き換えることになります。
/// 間引くので、書く回数は 1 セッションにつき多くても 60 秒に 1 回です。
///
/// 何もしないでいられる時間（放っておいて切れるまで）は、**期限からこの秒数を
/// 引いたぶん以上**が保証されます（既定の 7200 秒なら 7140 秒以上）。
/// ブラウザの Cookie の `Max-Age` は毎リクエスト延ばすので、置き場所の期限を
/// 大きく遅らせると、Cookie はあるのに中身が先に切れます。「残りが半分を切ったら
/// 延ばす」にしていたころは、期限の半分ほど放っておくだけでログアウトされていました。
///
/// 「最後に書いた時刻」は `expires_at - lifetime_secs` と見なします
/// （書くたびに `expires_at` を「いま + 期限」にしているため）。
fn needs_touch(expires_at: u64, lifetime_secs: u64) -> bool {
    let remaining = expires_at.saturating_sub(now_seconds_u64());
    let elapsed = lifetime_secs.saturating_sub(remaining);
    elapsed >= touch_slack(lifetime_secs)
}

/// 置き場所のディレクトリを、所有者だけが入れるようにする（Unix のみ）。
///
/// 失敗しても止めません。権限を変えられない置き場所（共有ディスクなど）もあります。
#[cfg(unix)]
fn restrict_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!("{} の権限を変えられませんでした: {e}", dir.display());
    }
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) {}

/// プロセスのメモリに置く。**テスト専用です。**
///
/// プロセスをまたげないので、本番では使えません
/// （本番はプロセスを 2 つ以上動かすことがあります。`docs/deployment.md`）。
pub struct MemoryStore {
    entries: Mutex<BTreeMap<String, (u64, Data)>>,
    lifetime_secs: u64,
    /// 「切った」印。利用者 ID → (ミリ秒, 除外した ID)。
    ///
    /// 秒ではなくミリ秒です。`revoked_at` が返す値はログイン時刻
    /// （`AUTH_AT_KEY`、ミリ秒）と比べるので、桁をそろえておきます。
    revoked: Mutex<BTreeMap<String, (i64, Option<String>)>>,
    /// 書き込みの回数。期限切れをまとめて落とす間隔を決めるのに使う。
    writes: std::sync::atomic::AtomicU64,
}

impl MemoryStore {
    /// 放っておいて消えるまでの秒数を決めて作る。
    pub fn new(lifetime_secs: u64) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            lifetime_secs,
            revoked: Mutex::new(BTreeMap::new()),
            writes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 期限切れの組をまとめて落とす（テストと `write` から呼ぶ）。
    fn collect_expired(entries: &mut BTreeMap<String, (u64, Data)>) {
        let cutoff = now_seconds_u64();
        entries.retain(|_, (expires_at, _)| *expires_at > cutoff);
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new(7200)
    }
}

impl SessionStore for MemoryStore {
    fn read(&self, id: &str) -> Result<Option<Data>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get(id) {
            Some((expires_at, _)) if *expires_at <= now_seconds_u64() => {
                entries.remove(id);
                Ok(None)
            }
            Some((_, data)) => Ok(Some(data.clone())),
            None => Ok(None),
        }
    }

    fn write(&self, id: &str, data: &Data) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let n = self
            .writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n % MEMORY_GC_EVERY == MEMORY_GC_EVERY - 1 {
            Self::collect_expired(&mut entries);
        }
        entries.insert(
            id.to_string(),
            (
                now_seconds_u64().saturating_add(self.lifetime_secs),
                data.clone(),
            ),
        );
        Ok(())
    }

    fn destroy(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(id);
        Ok(())
    }

    /// 錠を 1 回だけ取って、中身を返し、期限も延ばす。
    fn read_and_touch(&self, id: &str) -> Result<Option<Data>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some((expires_at, data)) = entries.get_mut(id) else {
            return Ok(None);
        };
        if *expires_at <= now_seconds_u64() {
            entries.remove(id);
            return Ok(None);
        }
        if needs_touch(*expires_at, self.lifetime_secs) {
            *expires_at = now_seconds_u64().saturating_add(self.lifetime_secs);
        }
        Ok(Some(data.clone()))
    }

    fn touch(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some((expires_at, _)) = entries.get_mut(id) else {
            return Ok(());
        };
        if *expires_at <= now_seconds_u64() || !needs_touch(*expires_at, self.lifetime_secs) {
            return Ok(());
        }
        *expires_at = now_seconds_u64().saturating_add(self.lifetime_secs);
        Ok(())
    }

    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, None)
    }

    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, Some(keep_id))
    }

    fn mark_revoked(&self, user_id: &str, except: Option<&str>) {
        let mut revoked = self.revoked.lock().unwrap_or_else(|e| e.into_inner());
        // 古い印はここで落とす（1 時間より前のものは誰にも効かない）。
        let now = crate::support::time::now_millis();
        let cutoff =
            now.saturating_sub(i64::try_from(REVOKED_MARK_SECS * 1000).unwrap_or(i64::MAX));
        revoked.retain(|_, (at, _)| *at >= cutoff);
        revoked.insert(user_id.to_string(), (now, except.map(str::to_string)));
    }

    fn revoked_at(&self, user_id: &str, session_id: &str) -> Option<i64> {
        let revoked = self.revoked.lock().unwrap_or_else(|e| e.into_inner());
        let (at, except) = revoked.get(user_id)?;
        if except.as_deref() == Some(session_id) {
            return None;
        }
        // 古い印は無視する（`FileStore` と同じ扱い）。`mark_revoked` が落とすのは
        // 次に誰かを切ったときなので、それまで残っている印をここで見ます。
        mark_is_fresh(*at).then_some(*at)
    }
}

impl MemoryStore {
    /// 利用者 ID が一致する組を消す。`keep` に渡した ID は残す。
    ///
    /// 全部を見ますが、メモリの上なので安いです。
    fn destroy_matching(&self, user_id: &str, keep: Option<&str>) -> Result<usize> {
        // 消す前に印を付ける（`FileStore` と同じ理由）。
        self.mark_revoked(user_id, keep);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let before = entries.len();
        entries.retain(|id, (_, data)| {
            if keep == Some(id.as_str()) {
                return true;
            }
            data.get(crate::auth::AUTH_ID_KEY).map(String::as_str) != Some(user_id)
        });
        Ok(before - entries.len())
    }
}

/// ファイルに書く形。
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    expires_at: u64,
    data: Data,
}

/// 期限切れのセッションを消す（`session:gc` から呼びます）。**全部見ます。**
///
/// 合わせて、5 分より古い書きかけの一時ファイルと、1 時間より古い「切った」印も消します。
pub(crate) fn sweep(dir: &Path) -> Result<usize> {
    sweep_limited(dir, None)
}

/// 期限切れのセッションを消す。消した**期限切れセッションの数**を返す。
///
/// `max` を渡すと、見るファイルの数をそこで打ち切ります。書き込みのついでの
/// 掃除（`maybe_sweep`）は上限つきで、`session:gc` は `None`（全部）です。
/// 上限つきのときは、見始める位置を毎回でたらめに変えます（[`SWEEP_MAX_ENTRIES`]）。
///
/// 返す数には**一時ファイルを数えません。** 数えると `session:gc` の
/// 「N 件消しました」が期限切れセッションの数と合わなくなります。
/// 一時ファイルの数はログに出します。
fn sweep_limited(dir: &Path, max: Option<usize>) -> Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::Io(e)),
    };
    // 名前の一覧だけを先に取ります（ファイルは開かない）。
    //
    // 上限つきのときは、**毎回でたらめな位置から**見始めて、末尾まで来たら
    // 先頭に戻ります。`read_dir` の順は毎回ほぼ同じなので、先頭から数えると
    // 同じ 500 件だけを見続け、それより後ろのファイルは永久に掃除されません。
    // 始める位置を毎回変えれば、どのファイルもいずれ見られます。
    let entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    let start = match max {
        Some(max) if entries.len() > max => random_index(entries.len()),
        _ => 0,
    };
    let take = max.unwrap_or(entries.len());
    if take < entries.len() {
        tracing::debug!(
            "セッションの掃除は {} 件のうち {take} 件だけ見ます（{start} 件目から）",
            entries.len()
        );
    }
    let mut removed = 0;
    let mut temps = 0;
    for index in window(entries.len(), start, take) {
        let entry = &entries[index];
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name == REVOKED_DIR {
            sweep_revoked_marks(&path);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        // 書きかけの一時ファイル（`<ID>.<乱数>.tmp`）は、まだ JSON として読めないので
        // 「読めない＝捨てる」の判定に掛けると `rename` の直前に消してしまいます。
        // 十分に古いもの（書き込みに失敗した残骸）だけ消します。
        if !is_session_id(&name) {
            if name.ends_with(".tmp")
                && crate::support::lock::is_stale(&path, TEMP_STALE_SECS)
                && std::fs::remove_file(&path).is_ok()
            {
                temps += 1;
            }
            continue;
        }
        let expired = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok())
            .map(|stored| stored.expires_at <= now_seconds_u64())
            // 読めないファイルも捨てる。
            .unwrap_or(true);
        if expired && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    if temps > 0 {
        tracing::info!("セッションの一時ファイルを {temps} 件片付けました");
    }
    Ok(removed)
}

/// `start` から `take` 件ぶんの位置を、末尾で先頭に戻りながら返す。
///
/// `take` が `len` より大きくても、同じ位置は 2 回返しません。
fn window(len: usize, start: usize, take: usize) -> impl Iterator<Item = usize> {
    (0..take.min(len)).map(move |offset| (start + offset) % len)
}

/// `0..len` のでたらめな位置。`len` は 1 以上で呼びます。
///
/// 8 バイトの乱数の余りなので、わずかに偏ります。掃除の始める位置に使うだけなので
/// かまいません。
fn random_index(len: usize) -> usize {
    let draw = u64::from_le_bytes(crypto::random_bytes::<8>());
    // `usize` が 64 ビットより狭い環境でも、余りは `len` 未満なので収まる。
    usize::try_from(draw % len as u64).unwrap_or(0)
}

/// 「切った」印の置き場所を掃除する。
///
/// 1 時間より古い印を消します。書きかけの一時ファイル（`<名前>.<乱数>.tmp`）は、
/// `sweep_limited` の本体と同じく**5 分より古いものだけ**消します。
/// 新しいものを消すと `mark_revoked` の `rename` が失敗します。
fn sweep_revoked_marks(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stale_after = if entry.file_name().to_string_lossy().ends_with(".tmp") {
            TEMP_STALE_SECS
        } else {
            REVOKED_MARK_SECS
        };
        if crate::support::lock::is_stale(&path, stale_after) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn data(pairs: &[(&str, &str)]) -> Data {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// テスト用の空ディレクトリ。
    ///
    /// プロセス ID と連番を混ぜます。名前だけだと、同じ worktree で `cargo test` を
    /// 2 本同時に走らせたときに取り合って落ちます。
    fn temp_dir(name: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bengara-session-test-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn メモリに置いて読み書きできる() {
        let store = MemoryStore::new(60);
        assert!(store.read("missing").unwrap().is_none());

        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("a", "1")])));

        store.destroy("abc").unwrap();
        assert!(store.read("abc").unwrap().is_none());
    }

    #[test]
    fn メモリの期限が切れたら読めない() {
        let store = MemoryStore::new(0);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert!(store.read("abc").unwrap().is_none(), "すぐ期限切れ");
    }

    #[test]
    fn ファイルに置いて読み書きできる() {
        let dir = temp_dir("rw");
        let store = FileStore::new(&dir, 60);

        assert!(store.read("abc").unwrap().is_none());
        store
            .write("abc", &data(&[("a", "1"), ("b", "2")]))
            .unwrap();
        assert!(dir.join("abc").is_file());
        assert_eq!(
            store.read("abc").unwrap(),
            Some(data(&[("a", "1"), ("b", "2")]))
        );

        store.destroy("abc").unwrap();
        assert!(store.read("abc").unwrap().is_none());
        // 2回消しても落ちない。
        store.destroy("abc").unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ファイルの期限が切れたら読めずに消える() {
        let dir = temp_dir("expire");
        let store = FileStore::new(&dir, 0);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert!(store.read("abc").unwrap().is_none());
        assert!(!dir.join("abc").exists(), "期限切れのファイルは消す");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 壊れたファイルは無かったことにする() {
        let dir = temp_dir("broken");
        let store = FileStore::new(&dir, 60);
        std::fs::write(dir.join("abc"), "これは JSON ではない").unwrap();
        assert!(store.read("abc").unwrap().is_none());
        assert!(!dir.join("abc").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 危ないidは断る() {
        let dir = temp_dir("unsafe-id");
        let store = FileStore::new(&dir, 60);
        for bad in ["", "../etc/passwd", "a/b", "a\\b", "a.b", &"x".repeat(129)] {
            assert!(store.read(bad).is_err(), "`{bad}` は断るはず");
            assert!(store.write(bad, &data(&[])).is_err());
        }
        // 正しい形は通る。
        assert!(store.read("abc123").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限切れだけを掃除する() {
        let dir = temp_dir("sweep");
        FileStore::new(&dir, 0)
            .write("old", &data(&[("a", "1")]))
            .unwrap();
        FileStore::new(&dir, 3600)
            .write("new", &data(&[("a", "1")]))
            .unwrap();

        let removed = sweep(&dir).unwrap();
        assert_eq!(removed, 1);
        assert!(!dir.join("old").exists());
        assert!(dir.join("new").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無い場所を掃除しても落ちない() {
        let dir = std::env::temp_dir().join("bengara-session-test-missing");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(sweep(&dir).unwrap(), 0);
    }

    /// 認証 ID だけを入れた中身を作る。
    fn with_user(user_id: &str) -> Data {
        data(&[(crate::auth::AUTH_ID_KEY, user_id), ("cart", "1")])
    }

    #[test]
    fn メモリで同じ利用者のセッションがまとめて消える() {
        let store = MemoryStore::new(60);
        store.write("a1", &with_user("7")).unwrap();
        store.write("a2", &with_user("7")).unwrap();
        store.write("b1", &with_user("8")).unwrap();
        // 認証 ID が入っていないセッションも混ぜる。
        store.write("anon", &data(&[("cart", "1")])).unwrap();

        assert_eq!(store.destroy_for_user("7").unwrap(), 2);
        assert!(store.read("a1").unwrap().is_none());
        assert!(store.read("a2").unwrap().is_none());
        assert!(store.read("b1").unwrap().is_some(), "他の利用者は残る");
        assert!(store.read("anon").unwrap().is_some(), "匿名も残る");
    }

    #[test]
    fn メモリで自分のセッションだけ残せる() {
        let store = MemoryStore::new(60);
        store.write("mine", &with_user("7")).unwrap();
        store.write("other", &with_user("7")).unwrap();
        store.write("b1", &with_user("8")).unwrap();

        assert_eq!(store.destroy_for_user_except("7", "mine").unwrap(), 1);
        assert!(store.read("mine").unwrap().is_some(), "自分は残る");
        assert!(store.read("other").unwrap().is_none(), "他の端末は切れる");
        assert!(store.read("b1").unwrap().is_some(), "他の利用者は残る");
    }

    #[test]
    fn ファイルで同じ利用者のセッションがまとめて消える() {
        let dir = temp_dir("destroy-user");
        let store = FileStore::new(&dir, 60);
        store.write("a1", &with_user("7")).unwrap();
        store.write("a2", &with_user("7")).unwrap();
        store.write("b1", &with_user("8")).unwrap();

        assert_eq!(store.destroy_for_user("7").unwrap(), 2);
        assert!(!dir.join("a1").exists());
        assert!(!dir.join("a2").exists());
        assert!(dir.join("b1").exists(), "他の利用者は残る");

        // 2回目は何も消さない。
        assert_eq!(store.destroy_for_user("7").unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ファイルで自分のセッションだけ残せる() {
        let dir = temp_dir("destroy-others");
        let store = FileStore::new(&dir, 60);
        store.write("mine", &with_user("7")).unwrap();
        store.write("other", &with_user("7")).unwrap();

        assert_eq!(store.destroy_for_user_except("7", "mine").unwrap(), 1);
        assert!(dir.join("mine").exists(), "自分は残る");
        assert!(!dir.join("other").exists(), "他の端末は切れる");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無い場所で利用者を消しても落ちない() {
        let dir = std::env::temp_dir().join("bengara-session-test-no-dir");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileStore::new(&dir, 60);
        assert_eq!(store.destroy_for_user("7").unwrap(), 0);
    }

    #[test]
    fn 既定の実装は何も消さない() {
        // 利用者が自分で作った置き場所が、足した2つのメソッドで壊れないこと。
        struct Bare;
        impl SessionStore for Bare {
            fn read(&self, _id: &str) -> Result<Option<Data>> {
                Ok(None)
            }
            fn write(&self, _id: &str, _data: &Data) -> Result<()> {
                Ok(())
            }
            fn destroy(&self, _id: &str) -> Result<()> {
                Ok(())
            }
        }
        let store = Bare;
        assert_eq!(store.destroy_for_user("7").unwrap(), 0);
        assert_eq!(store.destroy_for_user_except("7", "mine").unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn ファイルは所有者だけが読める() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("mode");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &with_user("7")).unwrap();
        let mode = std::fs::metadata(dir.join("abc"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "0{:o}", mode & 0o777);

        // 0644 で残っていたファイルも、書き直せば 0600 になる。
        let other = dir.join("def");
        std::fs::write(&other, "{}").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.write("def", &with_user("7")).unwrap();
        let mode = std::fs::metadata(&other).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "0{:o}", mode & 0o777);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn セッションidの形だけを通す() {
        assert!(is_session_id("abc123"));
        assert!(is_session_id("a-b"));
        assert!(!is_session_id(""));
        assert!(
            !is_session_id("a.b"),
            "ドットは通さない（一時ファイルの根拠）"
        );
        assert!(!is_session_id("a/b"));
        assert!(!is_session_id(".."));
        assert!(!is_session_id(&"x".repeat(129)));
    }

    #[test]
    fn 掃除は一時ファイルを触らない() {
        let dir = temp_dir("sweep-tmp");
        FileStore::new(&dir, 0)
            .write("old", &data(&[("a", "1")]))
            .unwrap();
        // 書きかけの一時ファイルを置いておく。JSON としては読めない。
        let temp = dir.join("abc.0123456789abcdef.tmp");
        std::fs::write(&temp, "{\"expires_at\":").unwrap();

        let removed = sweep(&dir).unwrap();
        assert_eq!(removed, 1, "期限切れの本体だけ消す");
        assert!(!dir.join("old").exists());
        assert!(
            temp.exists(),
            "書きかけは触らない（消すと rename が失敗する）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 同時に書き込んでも中身が混ざらない() {
        // 一時ファイル名が ID だけで決まっていたころは、同じ ID の2本が
        // 同じ `<ID>.tmp` を `truncate` で開き、混ざった JSON が本体になった。
        let dir = temp_dir("concurrent");
        let store = std::sync::Arc::new(FileStore::new(&dir, 60));

        let handles: Vec<_> = (0..8)
            .map(|n| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        // Windows では、同じ本体への `rename` が同時に走ると
                        // 「別のプロセスが使用中」で失敗することがあります。
                        // ここで確かめたいのは**中身が混ざらないこと**なので、
                        // 失敗そのものは見ません。
                        let _ = store.write("abc", &data(&[("n", "値")]));
                    }
                    n
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("スレッドが落ちていない");
        }

        // 本体は読める（混ざった JSON になっていれば `read` が消して `None`）。
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("n", "値")])));
        // 一時ファイルも残っていない。
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["abc".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 置き場所のファイルから `expires_at` を読む。
    fn expires_of(dir: &Path, id: &str) -> u64 {
        let raw = std::fs::read_to_string(dir.join(id)).unwrap();
        serde_json::from_str::<Stored>(&raw).unwrap().expires_at
    }

    #[test]
    fn 書いてから少し過ぎたらファイルの期限を延ばす() {
        let dir = temp_dir("touch-file");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        // 書いた直後は残り 60 秒。間引く秒数（期限の 1/10 = 6 秒）が過ぎていないので書かない。
        let before = expires_of(&dir, "abc");
        store.touch("abc").unwrap();
        assert_eq!(expires_of(&dir, "abc"), before, "まだ延ばさない");

        // 残り 10 秒の形に差し替える。
        let stored = Stored {
            expires_at: now_seconds_u64() + 10,
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();
        store.touch("abc").unwrap();
        let after = expires_of(&dir, "abc");
        assert!(after > now_seconds_u64() + 50, "期限が延びている");
        // 中身は変えない。
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("a", "1")])));

        // 置き場所に無い ID なら何もしない（新しいファイルを作らない）。
        store.touch("missing").unwrap();
        assert!(!dir.join("missing").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限切れのファイルは延命しない() {
        // `needs_touch` だけを見ていたころは、残り 0 秒が「半分を切った」に
        // 当たり、期限切れのセッションに満額の寿命を与え直していた。
        let dir = temp_dir("touch-expired");
        let store = FileStore::new(&dir, 60);
        let stored = Stored {
            expires_at: now_seconds_u64().saturating_sub(10),
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();

        store.touch("abc").unwrap();
        assert!(
            expires_of(&dir, "abc") <= now_seconds_u64(),
            "期限は延びない: {}",
            expires_of(&dir, "abc")
        );
        assert!(store.read("abc").unwrap().is_none(), "読めば消える");

        // メモリの置き場所も同じ（こちらには最初から確認があった）。
        let store = MemoryStore::new(60);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        store.entries.lock().unwrap().get_mut("abc").unwrap().0 =
            now_seconds_u64().saturating_sub(10);
        store.touch("abc").unwrap();
        assert!(store.read("abc").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 書いてから少し過ぎたらメモリの期限も延ばす() {
        let store = MemoryStore::new(60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        let before = store.entries.lock().unwrap().get("abc").unwrap().0;
        store.touch("abc").unwrap();
        assert_eq!(
            store.entries.lock().unwrap().get("abc").unwrap().0,
            before,
            "まだ延ばさない"
        );

        // 残り 10 秒にする。
        store.entries.lock().unwrap().get_mut("abc").unwrap().0 = now_seconds_u64() + 10;
        store.touch("abc").unwrap();
        assert!(
            store.entries.lock().unwrap().get("abc").unwrap().0 > now_seconds_u64() + 50,
            "期限が延びている"
        );
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("a", "1")])));

        // 置き場所に無い ID なら何もしない。
        store.touch("missing").unwrap();
        assert!(store.read("missing").unwrap().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn 既にあるディレクトリの権限も直す() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("perm");
        // tar の展開などで 0755 のまま置かれた形。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ディレクトリを他人から読めないようにする");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 書き換えても一時ファイルが残らない() {
        let dir = temp_dir("tmp");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        store.write("abc", &data(&[("a", "2")])).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["abc".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限の加算はあふれない() {
        let store = MemoryStore::new(u64::MAX);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert!(store.read("abc").unwrap().is_some(), "即時失効にならない");
        store.touch("abc").unwrap();

        let dir = temp_dir("overflow");
        let store = FileStore::new(&dir, u64::MAX);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert!(store.read("abc").unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 利用者のセッションを切ると印が残る() {
        for store in [
            Box::new(MemoryStore::new(60)) as Box<dyn SessionStore>,
            Box::new(FileStore::new(temp_dir("revoke"), 60)),
        ] {
            store
                .write("aaa", &data(&[(crate::auth::AUTH_ID_KEY, "7")]))
                .unwrap();
            store
                .write("bbb", &data(&[(crate::auth::AUTH_ID_KEY, "7")]))
                .unwrap();
            assert!(store.revoked_at("7", "aaa").is_none(), "切る前は印が無い");

            assert_eq!(store.destroy_for_user_except("7", "aaa").unwrap(), 1);
            let at = store.revoked_at("7", "bbb").expect("切った印がある");
            assert!(at >= now_seconds_u64() as i64 - 1);
            assert!(
                store.revoked_at("7", "aaa").is_none(),
                "残したセッションには印が効かない"
            );
            assert!(
                store.revoked_at("8", "bbb").is_none(),
                "別の利用者には効かない"
            );

            // 全部切ると、残した ID にも効く。
            assert_eq!(store.destroy_for_user("7").unwrap(), 1);
            assert!(store.revoked_at("7", "aaa").is_some());
        }
    }

    #[test]
    fn 掃除は印のディレクトリと新しい一時ファイルを壊さない() {
        let dir = temp_dir("sweep-extra");
        let store = FileStore::new(&dir, 60);
        store
            .write("aaa", &data(&[(crate::auth::AUTH_ID_KEY, "7")]))
            .unwrap();
        store.destroy_for_user("7").unwrap();
        std::fs::write(dir.join("aaa.deadbeef.tmp"), "{").unwrap();

        assert_eq!(sweep(&dir).unwrap(), 0);
        assert!(dir.join(REVOKED_DIR).is_dir(), "印の置き場所は残る");
        assert!(store.revoked_at("7", "zzz").is_some(), "新しい印は消えない");
        assert!(
            dir.join("aaa.deadbeef.tmp").is_file(),
            "新しい一時ファイルは残す"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 古い一時ファイルは掃除で消える() {
        let dir = temp_dir("sweep-tmp");
        let temp = dir.join("aaa.deadbeef.tmp");
        std::fs::write(&temp, "{").unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(TEMP_STALE_SECS + 60);
        std::fs::File::options()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_modified(old)
            .unwrap();

        // 返す数は**期限切れセッションの数**だけ。一時ファイルは数えない
        // （`session:gc` の「N 件消しました」を合わせるため）。
        assert_eq!(sweep(&dir).unwrap(), 0);
        assert!(!temp.exists(), "古い一時ファイルは消す");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 切った印は空に見えない() {
        // 直に書いていたころは `truncate` で開くので、
        // 中身が 0 バイトの瞬間があった。その瞬間に `revoked_at` が読むと
        // 「切られていない」と答え、切ったはずのセッションが保存される。
        let dir = temp_dir("revoke-atomic");
        let store = std::sync::Arc::new(FileStore::new(&dir, 60));
        store.mark_revoked("7", None);
        let mark = store.revoked_path_of("7");

        let writer = {
            let store = store.clone();
            std::thread::spawn(move || {
                for _ in 0..300 {
                    store.mark_revoked("7", None);
                }
            })
        };
        // 読めたときは必ず中身がそろっている。Windows では置き換えの最中に
        // 開けないことがあるので、**開けたかどうかは見ません。**
        for _ in 0..300 {
            if let Ok(raw) = std::fs::read_to_string(&mark) {
                let at = raw
                    .lines()
                    .next()
                    .and_then(|line| line.trim().parse::<i64>().ok());
                assert!(at.is_some(), "中身が空に見えた: {raw:?}");
            }
        }
        writer.join().expect("スレッドが落ちていない");

        assert!(store.revoked_at("7", "aaa").is_some(), "印は残っている");
        // 一時ファイルも残らない。
        let left: Vec<String> = std::fs::read_dir(dir.join(REVOKED_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "一時ファイルが残っている: {left:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 掃除は一度に見る数を打ち切る() {
        // 上限が無いと、抽選に当たった 1 回が置き場所の全ファイルを解析する。
        // `store.write` は抽選つきなので、ここではファイルを直に置く。
        let dir = temp_dir("sweep-max");
        let stored = Stored {
            expires_at: now_seconds_u64().saturating_sub(10),
            data: data(&[("a", "1")]),
        };
        let body = serde_json::to_string(&stored).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("id{i}")), &body).unwrap();
        }

        assert_eq!(sweep_limited(&dir, Some(2)).unwrap(), 2, "2 件で打ち切る");
        assert_eq!(
            sweep_limited(&dir, None).unwrap(),
            3,
            "上限なしなら残りも消える"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "期限切れは残らない"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 古い印は効かない() {
        // 読むときに古さを見ていなかったころは、掃除が走るまで
        // （メモリでは次に誰かを切るまで）印が効き続けた。
        let now_ms = crate::support::time::now_millis();
        let old = now_ms - i64::try_from(REVOKED_MARK_SECS * 1000).unwrap() - 1_000;
        assert!(!mark_is_fresh(old), "1 時間より古い");
        assert!(mark_is_fresh(now_ms), "いま付けた印は効く");
        assert!(
            mark_is_fresh(now_ms + 5_000),
            "時計が戻っても効くほうに倒す"
        );

        // ファイル。印のファイルを古い時刻で直に置く。
        let dir = temp_dir("revoke-stale");
        let store = FileStore::new(&dir, 60);
        std::fs::create_dir_all(dir.join(REVOKED_DIR)).unwrap();
        std::fs::write(store.revoked_path_of("7"), format!("{old}\n")).unwrap();
        assert!(store.revoked_at("7", "aaa").is_none(), "古い印は無視する");
        store.mark_revoked("7", None);
        assert!(store.revoked_at("7", "aaa").is_some(), "新しい印は効く");
        let _ = std::fs::remove_dir_all(&dir);

        // メモリ。
        let store = MemoryStore::new(60);
        store
            .revoked
            .lock()
            .unwrap()
            .insert("7".to_string(), (old, None));
        assert!(store.revoked_at("7", "aaa").is_none(), "古い印は無視する");
        store.mark_revoked("7", None);
        assert!(store.revoked_at("7", "aaa").is_some(), "新しい印は効く");
    }

    #[test]
    fn ファイルは一度読むだけで期限も延ばす() {
        let dir = temp_dir("read-touch-file");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        // 書いた直後は残り 60 秒。読めるが、まだ延ばさない。
        let before = expires_of(&dir, "abc");
        assert_eq!(
            store.read_and_touch("abc").unwrap(),
            Some(data(&[("a", "1")]))
        );
        assert_eq!(expires_of(&dir, "abc"), before, "まだ延ばさない");

        // 残り 10 秒の形に差し替えると、読むついでに延びる。中身は変えない。
        let stored = Stored {
            expires_at: now_seconds_u64() + 10,
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();
        assert_eq!(
            store.read_and_touch("abc").unwrap(),
            Some(data(&[("a", "1")]))
        );
        assert!(
            expires_of(&dir, "abc") > now_seconds_u64() + 50,
            "期限が延びている"
        );

        // 期限切れは延命せず、`read` と同じく消して `None`。
        let stored = Stored {
            expires_at: now_seconds_u64().saturating_sub(10),
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();
        assert!(store.read_and_touch("abc").unwrap().is_none());
        assert!(!dir.join("abc").exists(), "期限切れのファイルは消す");

        // 無い ID は `None`。新しいファイルも作らない。
        assert!(store.read_and_touch("missing").unwrap().is_none());
        assert!(!dir.join("missing").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn メモリも一度読むだけで期限を延ばす() {
        let store = MemoryStore::new(60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        let before = store.entries.lock().unwrap().get("abc").unwrap().0;
        assert_eq!(
            store.read_and_touch("abc").unwrap(),
            Some(data(&[("a", "1")]))
        );
        assert_eq!(
            store.entries.lock().unwrap().get("abc").unwrap().0,
            before,
            "まだ延ばさない"
        );

        store.entries.lock().unwrap().get_mut("abc").unwrap().0 = now_seconds_u64() + 10;
        assert!(store.read_and_touch("abc").unwrap().is_some());
        assert!(
            store.entries.lock().unwrap().get("abc").unwrap().0 > now_seconds_u64() + 50,
            "期限が延びている"
        );

        // 期限切れは延命せず、消して `None`。
        store.entries.lock().unwrap().get_mut("abc").unwrap().0 =
            now_seconds_u64().saturating_sub(10);
        assert!(store.read_and_touch("abc").unwrap().is_none());
        assert!(store.entries.lock().unwrap().get("abc").is_none());
        assert!(store.read_and_touch("missing").unwrap().is_none());
    }

    #[test]
    fn 既定の読むついでは読んでから延ばす() {
        // 自分で作った置き場所は、足したメソッドを書かなくても今までどおり動く
        // （`read` のあとに `touch`。`touch` の既定は読んで書き直す）。
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct Counting {
            reads: AtomicUsize,
            writes: AtomicUsize,
            fail_write: bool,
        }
        impl SessionStore for Counting {
            fn read(&self, id: &str) -> Result<Option<Data>> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                Ok((id == "abc").then(|| data(&[("a", "1")])))
            }
            fn write(&self, _id: &str, _data: &Data) -> Result<()> {
                self.writes.fetch_add(1, Ordering::Relaxed);
                if self.fail_write {
                    return Err(Error::msg("書けない"));
                }
                Ok(())
            }
            fn destroy(&self, _id: &str) -> Result<()> {
                Ok(())
            }
        }

        let store = Counting::default();
        assert_eq!(
            store.read_and_touch("abc").unwrap(),
            Some(data(&[("a", "1")]))
        );
        assert_eq!(store.writes.load(Ordering::Relaxed), 1, "延ばしている");

        // 無い ID では延ばさない。
        assert!(store.read_and_touch("missing").unwrap().is_none());
        assert_eq!(store.writes.load(Ordering::Relaxed), 1);

        // 延ばせなくても、読めた中身は返す（警告だけ）。
        let store = Counting {
            fail_write: true,
            ..Counting::default()
        };
        assert_eq!(
            store.read_and_touch("abc").unwrap(),
            Some(data(&[("a", "1")]))
        );
    }

    #[test]
    fn 切られた後は期限を延ばさない() {
        // 「読む → 書く」のあいだに切られると、消したはずのセッションを
        // 書き戻す。書く直前に印を見て、切られていれば書かない。
        let dir = temp_dir("touch-revoked");
        let store = FileStore::new(&dir, 60);
        let logged_in = data(&[
            (crate::auth::AUTH_ID_KEY, "7"),
            (crate::auth::AUTH_AT_KEY, "1000"),
        ]);
        let stored = Stored {
            expires_at: now_seconds_u64() + 10,
            data: logged_in.clone(),
        };
        let body = serde_json::to_string(&stored).unwrap();
        std::fs::write(dir.join("abc"), &body).unwrap();
        let before = expires_of(&dir, "abc");

        // 印だけ付ける（ファイルを消す直前の状態）。
        store.mark_revoked("7", None);
        assert_eq!(store.read_and_touch("abc").unwrap(), Some(logged_in));
        assert_eq!(expires_of(&dir, "abc"), before, "書き戻さない");
        store.touch("abc").unwrap();
        assert_eq!(expires_of(&dir, "abc"), before, "`touch` も書き戻さない");

        // 印の除外に当たる ID（切った本人）は延ばす。
        store.mark_revoked("7", Some("abc"));
        store.touch("abc").unwrap();
        assert!(
            expires_of(&dir, "abc") > now_seconds_u64() + 50,
            "本人は延ばす"
        );

        // ログインしていない中身は印と関係ない。
        std::fs::write(
            dir.join("anon"),
            serde_json::to_string(&Stored {
                expires_at: now_seconds_u64() + 10,
                data: data(&[("cart", "1")]),
            })
            .unwrap(),
        )
        .unwrap();
        store.touch("anon").unwrap();
        assert!(expires_of(&dir, "anon") > now_seconds_u64() + 50);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn メモリの置き場所は期限切れをまとめて落とす() {
        let store = MemoryStore::new(0);
        for i in 0..MEMORY_GC_EVERY {
            store
                .write(&format!("id{i}"), &data(&[("a", "1")]))
                .unwrap();
        }
        let len = store.entries.lock().unwrap().len();
        assert!(len <= 1, "期限切れ（寿命 0 秒）が残らない: {len}");
    }

    #[test]
    fn 延ばす間隔は60秒か期限の10分の1の短いほう() {
        assert_eq!(touch_slack(7200), 60);
        assert_eq!(touch_slack(300), 30);
        assert_eq!(touch_slack(5), 0, "とても短い期限では毎回延ばす");

        let now = now_seconds_u64();
        // 既定の 7200 秒。書いてから 59 秒なら書かない、60 秒なら書く。
        assert!(!needs_touch(now + 7200 - 59, 7200));
        assert!(needs_touch(now + 7200 - 60, 7200));
        // 「残りが半分を切ったら」だったころは、ここ（残り 6000 秒）で書かず、
        // Cookie だけ延びて置き場所の期限が先に切れた。
        assert!(needs_touch(now + 6000, 7200));
        assert!(needs_touch(now, 7200), "期限ちょうども延ばす側");
    }

    #[test]
    fn 放っておける時間は期限からほとんど減らない() {
        // 最後に延ばしてから間引く秒数ぶん経ったところで延ばすので、
        // そのあと何もしなくても「期限 - 間引く秒数」以上は残る。
        let store = MemoryStore::new(7200);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        // 61 秒前に書いた形にする（残り 7139 秒）。
        store.entries.lock().unwrap().get_mut("abc").unwrap().0 = now_seconds_u64() + 7200 - 61;
        store.read_and_touch("abc").unwrap();
        let expires_at = store.entries.lock().unwrap().get("abc").unwrap().0;
        assert!(expires_at >= now_seconds_u64() + 7200 - 1, "満額に戻る");

        // 書いた直後は延ばさない（書く回数を抑える）。
        let dir = temp_dir("touch-slack");
        let file = FileStore::new(&dir, 7200);
        file.write("abc", &data(&[("a", "1")])).unwrap();
        let stored = Stored {
            expires_at: now_seconds_u64() + 7200 - 30,
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();
        file.read_and_touch("abc").unwrap();
        assert_eq!(
            expires_of(&dir, "abc"),
            stored.expires_at,
            "30 秒では書かない"
        );

        let stored = Stored {
            expires_at: now_seconds_u64() + 7200 - 61,
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();
        file.read_and_touch("abc").unwrap();
        assert!(
            expires_of(&dir, "abc") >= now_seconds_u64() + 7200 - 1,
            "61 秒なら延ばす"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 見る範囲は末尾で先頭に戻る() {
        assert_eq!(window(5, 3, 3).collect::<Vec<_>>(), vec![3, 4, 0]);
        assert_eq!(window(5, 0, 2).collect::<Vec<_>>(), vec![0, 1]);
        // 上限が数より多くても、同じ位置を 2 回見ない。
        assert_eq!(window(3, 2, 10).collect::<Vec<_>>(), vec![2, 0, 1]);
        assert_eq!(window(0, 0, 10).count(), 0);
        for _ in 0..100 {
            assert!(random_index(7) < 7);
        }
        assert_eq!(random_index(1), 0);
    }

    #[test]
    fn 上限つきの掃除を繰り返せば全部に届く() {
        // 先頭から数えていたころは、毎回同じ 2 件だけを見て、残りは永久に残った。
        let dir = temp_dir("sweep-rotate");
        let body = |expires_at: u64| {
            serde_json::to_string(&Stored {
                expires_at,
                data: data(&[("a", "1")]),
            })
            .unwrap()
        };
        // 生きているファイル 9 件と、期限切れ 1 件。
        for i in 0..9 {
            std::fs::write(
                dir.join(format!("alive{i}")),
                body(now_seconds_u64() + 3600),
            )
            .unwrap();
        }
        let expired = dir.join("zzexpired");
        std::fs::write(&expired, body(now_seconds_u64().saturating_sub(10))).unwrap();

        // 1 回に 1 件しか見なくても、始める位置が毎回変わるので、いずれ当たる。
        // 外れ続ける確率は (9/10)^500 で、事実上 0。
        for _ in 0..500 {
            sweep_limited(&dir, Some(1)).unwrap();
            if !expired.exists() {
                break;
            }
        }
        assert!(!expired.exists(), "期限切れに届く");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            9,
            "生きているものは残る"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
