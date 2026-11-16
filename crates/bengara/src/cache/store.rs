//! キャッシュの置き場所。
//!
//! 既定は `FileCache`（`storage/framework/cache/`）です。
//! テストやディスクを使いたくないときは `MemoryCache` を使えます。
//!
//! 形は `SessionStore`（Phase 2）にそろえてあります。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::support::fs::{write_atomic, Access};
use crate::support::lock::{is_stale, FileLock};
use crate::support::time;
use crate::support::MEMORY_GC_EVERY;

/// キャッシュの置き場所。
///
/// 自分で作るときは、次の3つを満たしてください。
///
/// - **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
/// - 期限切れは読み込みのときに捨ててください。
/// - 読み書きはブロックしてかまいません（`spawn_blocking` の中で呼びます）。
///
/// `increment` 以下には既定の実装が付いています。足しても、既にある実装は壊れません。
pub trait CacheStore: Send + Sync + 'static {
    /// 入れる。`seconds` が `None` なら期限なし。
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()>;

    /// 読む。無い・期限切れは `None`。
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// 消す。
    fn forget(&self, key: &str) -> Result<()>;

    /// 全部消す。
    fn flush(&self) -> Result<()>;

    /// あるか。
    ///
    /// 既定の実装は値を読んで捨てます。値が大きいときは自分で実装してください。
    fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// 数を足して、足した後の値を返す。無ければ 0 から始めます。
    ///
    /// **既定の実装は「読む → 足す → 書く」の3手に分かれているので、
    /// 同時に呼ばれると数を落とします。** 正しく数えたい置き場所は、
    /// 自分で排他を取って実装してください。
    ///
    /// **期限は引き継ぎません**（期限なしで書き直します）。回数の記録に使う前提です。
    fn increment(&self, key: &str, by: i64) -> Result<i64> {
        let current = parse_number(key, self.get(key)?)?;
        // あふれさせません。`current + by` はデバッグビルドでパニックし
        // （裏のスレッドが落ちて 500 になります）、リリースでは巻き戻ります。
        // `Entry::encode` の期限と同じく、上限・下限で止めます。
        let next = current.saturating_add(by);
        self.put(key, &next.to_string(), None)?;
        Ok(next)
    }

    /// 読み書きがブロックするか。
    ///
    /// `true`（既定）のときだけ、裏のスレッド（`spawn_blocking`）を経由します。
    /// メモリだけで済む置き場所は `false` にすると、無駄な往復がなくなります。
    fn blocking(&self) -> bool {
        true
    }

    /// 期限切れのものだけ消して、消した件数を返す（`cache:prune`）。
    ///
    /// 既定の実装は何もしません。[`CacheStore::prunable`] も合わせて `true` にしてください。
    fn prune(&self) -> Result<usize> {
        Ok(0)
    }

    /// 期限切れの掃除ができるか。`cache:prune` の表示を変えるために見ます。
    fn prunable(&self) -> bool {
        false
    }

    /// 置き場所を人に見せる言い方（コマンドの表示用）。言えないときは `None`。
    fn location(&self) -> Option<String> {
        None
    }
}

/// キャッシュに入っている文字を数として読む。
fn parse_number(key: &str, raw: Option<String>) -> Result<i64> {
    match raw {
        Some(found) => found
            .trim()
            .parse::<i64>()
            .map_err(|_| Error::msg(format!("`{key}` の値は数ではありません"))),
        None => Ok(0),
    }
}

/// 1件ぶんの中身。ファイルにはこの形で入ります。
///
/// `期限|値` の 1 行です。期限が `0` なら期限なしです。
/// JSON にしないのは、値の中に改行があっても壊れないようにするためです。
struct Entry {
    expires_at: i64,
    value: String,
}

impl Entry {
    fn encode(value: &str, seconds: Option<u64>) -> String {
        let expires_at = match seconds {
            // **あふれさせません。** `as i64` だと `u64::MAX` が `-1` になって
            // 即時失効し、`i64::MAX` の近くはデバッグビルドでパニックします。
            // 上限で切り詰めてから飽和加算します。
            Some(seconds) => {
                let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                time::now_seconds().saturating_add(seconds)
            }
            None => 0,
        };
        format!("{expires_at}|{value}")
    }

    fn decode(raw: &str) -> Option<Self> {
        let (expires, value) = raw.split_once('|')?;
        Some(Self {
            expires_at: expires.trim().parse().ok()?,
            value: value.to_string(),
        })
    }

    fn expired(&self) -> bool {
        self.expires_at != 0 && self.expires_at <= time::now_seconds()
    }
}

/// 書き込み途中の一時ファイルに付ける連番。
///
/// 鍵が違えば一時ファイルも違う名前になるように、プロセス番号と合わせて使います。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 一時ファイルの印。**`normalize_key` が絶対に作らない形**にします。
///
/// `normalize_key` が通すのは小文字英数字と `. _ -`（とハッシュ側の先頭の `~`）
/// だけなので、`+` は出てきません。
/// 「後ろ2つが数字」で見分けていたときは、`report.2024.12` のような鍵の本体を
/// 一時ファイルと取り違えていました（`cache:clear` で消えず、`cache:prune` では
/// 期限が残っていても消えていました）。
const TEMP_MARK: &str = ".+tmp.";

/// 錠ファイルを置くディレクトリの名前。**`normalize_key` が絶対に作らない形**にします。
///
/// `locks` のような普通の名前だと、鍵 `locks` の本体とぶつかって
/// `Cache::get("locks")` がディレクトリを読もうとして必ずエラーになりました。
/// 大文字（`LOCKS`）では直りません。NTFS と APFS は大文字小文字を区別しないので、
/// Windows と macOS では鍵 `locks` と同じ名前のままです。
const LOCK_DIR: &str = "+locks";

/// 一時ファイルがこの秒数より古ければ、書き込みに失敗した残骸と見なして消す。
const TEMP_STALE_SECS: u64 = 300;

/// `storage/framework/cache/` にファイルとして置く。既定の置き場所です。
///
/// 1つの鍵が1つのファイルになります。
/// プロセスを2つ以上動かすときは、全プロセスで同じ場所を指してください。
pub struct FileCache {
    dir: PathBuf,
}

impl FileCache {
    /// 置き場所を決めて作る。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `storage/framework/cache/` に置く（既定）。
    pub fn default_path() -> Self {
        Self::new(Self::default_dir())
    }

    /// 既定の置き場所。**パスの定義はここ1か所だけです。**
    fn default_dir() -> PathBuf {
        // join を 2 回に分けて、Windows でも区切りが混ざらないようにする。
        crate::paths::storage_path("framework").join("cache")
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    /// 書き込み途中に使う名前。
    ///
    /// `<鍵>.+tmp.<プロセス番号>.<連番>` にします。`with_extension("tmp")` は
    /// 最後の `.` 以降を置き換えるので、`test.put` と `test.count` が同じ
    /// `test.tmp` を共有してしまいます。プロセス番号と連番を足して、
    /// 鍵をまたいでも重ならない名前にします。
    ///
    /// 印（[`TEMP_MARK`]）を入れるのは、利用者の鍵と確実に見分けるためです。
    fn temp_path_of(&self, key: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        self.dir
            .join(format!("{key}{TEMP_MARK}{}.{seq}", std::process::id()))
    }

    /// 1つの鍵を守る錠ファイルの場所。
    ///
    /// 本体と混ざらないよう、別のディレクトリ（[`LOCK_DIR`]）に置きます
    /// （`read_dir` ではファイルだけを見ているので、掃除の対象にもなりません）。
    fn lock_path_of(&self, key: &str) -> PathBuf {
        self.dir.join(LOCK_DIR).join(format!("{key}.lock"))
    }

    fn ensure_dir(&self) -> Result<()> {
        if self.dir.is_dir() {
            return Ok(());
        }
        // storage/ そのものが無いなら、作らずに知らせる（決定記録 #023）。
        let storage = crate::paths::storage_path("");
        if !storage.is_dir() {
            return Err(Error::msg(crate::paths::missing_storage_message(&storage)));
        }
        std::fs::create_dir_all(&self.dir)?;
        Ok(())
    }
}

impl CacheStore for FileCache {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> {
        self.ensure_dir()?;
        let path = self.path_of(key);
        // 書いている途中の中身を読まれないよう、別の名前で書いてから置き換える。
        let temp = self.temp_path_of(key);
        // Windows の「使用中」のやり直しと、失敗時の片付けは `write_atomic` が持ちます。
        let body = Entry::encode(value, seconds);
        write_atomic(&temp, &path, body.as_bytes(), Access::Default)?;
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let path = self.path_of(key);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => Some(raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // UTF-8 でない中身。`put` は文字列しか書かないので、これも壊れたファイルです。
            // エラーで返すと、`has` は「無い」と答えるのに `get` だけ 500 になりました。
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => None,
            Err(e) => return Err(Error::Io(e)),
        };
        // **壊れていても期限切れでも、ここでは消しません。** 読んでから消すまでの
        // 間に別の `put` が新しい中身へ置き換えると、その新しい値を消していました。
        // `increment` の数も、そこで 0 に戻って落ちました。
        // 片付けは `cache:prune` が、置き換わっていないかを確かめてから行います。
        let Some(entry) = raw.as_deref().and_then(Entry::decode) else {
            // 壊れていたら無かったことにする。落とすほどのことではない。
            tracing::warn!(
                "キャッシュのファイルを読めませんでした（cache:prune で片付きます）: {}",
                path.display()
            );
            return Ok(None);
        };
        if entry.expired() {
            return Ok(None);
        }
        Ok(Some(entry.value))
    }

    fn forget(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.path_of(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// 置き場所のファイルを消す。
    ///
    /// **消すのはファイルだけです。** 錠のディレクトリ（`+locks/`）は
    /// ディレクトリなので残ります。中の錠ファイルも消しません。`increment` が
    /// 握っている最中の錠を消すと、排他が外れて数を落とします。
    /// 錠ファイルは手放したときに消えるので、たまるのは空のディレクトリ 1 つだけです。
    fn flush(&self) -> Result<()> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Io(e)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // 別のプロセスが書いている途中の一時ファイルは消さない。
            // 消すと、向こうの `rename` が NotFound で失敗します。
            if is_temp_path(&path) && !is_stale(&path, TEMP_STALE_SECS) {
                continue;
            }
            let _ = std::fs::remove_file(&path);
        }
        Ok(())
    }

    /// 期限だけを読んで答える。**値は読みません。**
    ///
    /// `get` を通すと、`put_json` で入れた大きな構造体まで捨てるためだけに
    /// 読み込んでいました。期限は `期限|値` の `|` より前にしかないので、
    /// `read_expires_at` で先頭だけ見れば足ります。
    ///
    /// **期限切れのファイルはその場で消しません**（`get` も消しません）。
    /// 「あるか」を聞かれただけで書き込みを起こさないほうが素直です。
    /// 片づけは `cache:prune` が引き受けます。
    fn has(&self, key: &str) -> Result<bool> {
        let Some(expires_at) = read_expires_at(&self.path_of(key)) else {
            // 無い・読めない・壊れている。どれも「無い」とします。
            return Ok(false);
        };
        Ok(expires_at == 0 || expires_at > time::now_seconds())
    }

    /// 錠を取ってから「読む → 足す → 書く」を行う。
    ///
    /// 錠は**鍵ごとの錠ファイル1つだけ**です。`create_new(true)` で作るので、
    /// 作れた側だけが進めます。これはプロセスをまたぐ分にも、同じプロセスの
    /// スレッド同士にも効きます。
    ///
    /// プロセス全体で1つの `Mutex` は**持ちません**。握ったまま錠ファイルを
    /// 待つので、別プロセスが錠を持つ間、無関係な鍵の `increment` まで止まりました。
    ///
    /// **期限は引き継ぎません**（期限なしで書き直します）。Laravel の
    /// `increment` は元の期限を保つので、そこは挙動が違います。
    fn increment(&self, key: &str, by: i64) -> Result<i64> {
        self.ensure_dir()?;
        let _file = FileLock::acquire(&self.lock_path_of(key))?;

        let current = parse_number(key, self.get(key)?)?;
        // あふれさせません（既定の実装と同じ理由）。
        let next = current.saturating_add(by);
        self.put(key, &next.to_string(), None)?;
        Ok(next)
    }

    fn prune(&self) -> Result<usize> {
        sweep_expired(self)
    }

    fn prunable(&self) -> bool {
        true
    }

    fn location(&self) -> Option<String> {
        Some(self.dir.display().to_string())
    }
}

/// プロセスのメモリに置く。**プロセスをまたいで共有されません。**
///
/// テストと、ディスクを使いたくないときに使います。
#[derive(Default)]
pub struct MemoryCache {
    entries: Mutex<HashMap<String, String>>,
    /// 書き込みの回数。期限切れをまとめて落とす間隔を決めるのに使う。
    writes: AtomicU64,
}

impl MemoryCache {
    /// 空の置き場所を作る。
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 期限切れをまとめて落とす。
    ///
    /// `get` は読んだ鍵しか消さないので、二度と読まれない鍵が増え続けます。
    /// 書き込みの 100 回に 1 回ここを通します。
    fn collect_expired(entries: &mut HashMap<String, String>) {
        entries.retain(|_, raw| Entry::decode(raw).is_some_and(|entry| !entry.expired()));
    }
}

impl CacheStore for MemoryCache {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> {
        let mut entries = self.lock();
        let n = self.writes.fetch_add(1, Ordering::Relaxed);
        if n % MEMORY_GC_EVERY == MEMORY_GC_EVERY - 1 {
            Self::collect_expired(&mut entries);
        }
        entries.insert(key.to_string(), Entry::encode(value, seconds));
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let mut entries = self.lock();
        let Some(raw) = entries.get(key) else {
            return Ok(None);
        };
        let Some(entry) = Entry::decode(raw) else {
            entries.remove(key);
            return Ok(None);
        };
        if entry.expired() {
            entries.remove(key);
            return Ok(None);
        }
        Ok(Some(entry.value))
    }

    fn forget(&self, key: &str) -> Result<()> {
        self.lock().remove(key);
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.lock().clear();
        Ok(())
    }

    /// Mutex の中で読んで書く。数を落としません。
    ///
    /// **期限は引き継ぎません**（期限なしで書き直します）。
    fn increment(&self, key: &str, by: i64) -> Result<i64> {
        let mut entries = self.lock();
        let current = match entries.get(key).and_then(|raw| Entry::decode(raw)) {
            // 期限切れは 0 から数え直す。
            Some(entry) if !entry.expired() => entry
                .value
                .trim()
                .parse::<i64>()
                .map_err(|_| Error::msg(format!("`{key}` の値は数ではありません")))?,
            _ => 0,
        };
        // あふれさせません（`CacheStore::increment` の既定の実装と同じ理由）。
        let next = current.saturating_add(by);
        entries.insert(key.to_string(), Entry::encode(&next.to_string(), None));
        Ok(next)
    }

    /// メモリだけなのでブロックしません（裏のスレッドを経由しません）。
    fn blocking(&self) -> bool {
        false
    }
}

/// 書き込み途中の一時ファイルの名前か。
///
/// `put` が付ける印（[`TEMP_MARK`]）で見分けます。`normalize_key` は `+` を
/// 通さないので、利用者の鍵がこの印を含むことはありません。
///
/// 「後ろ2つが数字」で見ていたときは、`report.2024.12.tmp` や `page.1.2.tmp`
/// のような鍵の本体を一時ファイルと取り違えていました。
fn is_temp_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.contains(TEMP_MARK))
}

/// 期限だけを読む。読めない・壊れているときは `None`。
///
/// 期限は `期限|値` の `|` より前にしかありません。`read_to_string` だと、
/// `put_json` で入れた大きな構造体まで**捨てるためだけに**読み込みます。
///
/// **先頭 32 バイトしか読みません。** [`Entry::encode`] は期限（`i64` なので
/// 符号を入れて最大 20 文字）と `|` を先に書くので、`|` は 21 バイト目までに
/// 必ず来ます。`BufReader` は通しません。既定の 8 KiB のバッファを確保して、
/// ディスクからも最大 8 KiB 読んでいました。
fn read_expires_at(path: &Path) -> Option<i64> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 32];
    let mut filled = 0;
    while filled < head.len() {
        // `read` は頼んだより少なく返すことがあるので、`|` が出るまで繰り返す。
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
        if head[..filled].contains(&b'|') {
            break;
        }
    }
    let head = &head[..filled];
    let end = head.iter().position(|b| *b == b'|')?;
    std::str::from_utf8(&head[..end]).ok()?.trim().parse().ok()
}

/// 期限切れのファイルを消して、消した件数を返す。
///
/// 壊れたファイルも期限切れとして数えます。古い一時ファイル（書き込みに
/// 失敗した残骸）は消しますが、件数には入れません。
///
/// 呼び出し元は [`FileCache::prune`]（`cache:prune`）だけです。
/// `cache:clear` は `flush()` を通るので、ここは通りません。
///
/// 1 件ずつ [`remove_if_expired`] で消します。「期限切れだと読む」と
/// 「消す」の間に `put` が新しい中身へ置き換えると、新しい値を消して
/// しまうためです。
fn sweep_expired(cache: &FileCache) -> Result<usize> {
    let dir = cache.dir.as_path();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if is_temp_path(&path) {
            // 書き込み中かもしれないので**読まない**。十分古いものだけ残骸として消す。
            // 読んで「壊れている」と判断して消すと、向こうの `rename` が失敗します。
            //
            // **件数には入れません。** `cache:prune` は「期限切れのキャッシュを n 件」と
            // 出すので、書き込みに失敗した残骸を混ぜると数が合いません。
            if is_stale(&path, TEMP_STALE_SECS) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        // 期限は 1 行目の `|` より前にしかない。値は読まない。
        // ここでは候補を選ぶだけで、消すかどうかは `remove_if_expired` が決め直す。
        if !expired_at(&path) {
            continue;
        }
        let Some(key) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if remove_if_expired(cache, key) {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 期限切れか、壊れているか（どちらも消してよいもの）。
fn expired_at(path: &Path) -> bool {
    read_expires_at(path)
        .is_none_or(|expires_at| expires_at != 0 && expires_at <= time::now_seconds())
}

/// 錠を取る回数（1 回 10ms）。掃除は急がないので、すぐ諦めます。
const SWEEP_LOCK_TRIES: u32 = 3;

/// 鍵 1 つを、**置き換わっていなければ**消す。消したら真。
///
/// 次の 2 つで守ります。
///
/// | 相手 | 守り方 |
/// |---|---|
/// | `increment` | 同じ鍵の錠（`+locks/`）を取る。取れなければ使用中なので消さない |
/// | `put`（錠を取らない） | 一時の名前へ退避してから中身を見直す。新しければ戻す |
///
/// 退避の `rename` は 1 回で済むので、「調べる → 消す」の間に `put` の
/// `rename` が割り込んでも、退避したものを見れば分かります。戻すときは
/// `hard_link` を使います。戻す先に既にファイルがあれば（その間にさらに
/// `put` があれば）失敗するので、より新しい値を上書きしません。
///
/// 退避している一瞬は、その鍵の `get` が「無い」を返します。キャッシュなので
/// 取りこぼしとして扱えます（作り直されるだけです）。
fn remove_if_expired(cache: &FileCache, key: &str) -> bool {
    let path = cache.path_of(key);
    let Ok(_lock) = FileLock::acquire_tries(&cache.lock_path_of(key), SWEEP_LOCK_TRIES) else {
        // `increment` の最中。いま書いている鍵は期限切れではない。
        return false;
    };
    // 錠を取るまでに置き換わっているかもしれないので、見直す。
    if !expired_at(&path) {
        return false;
    }
    let tomb = cache.temp_path_of(key);
    if std::fs::rename(&path, &tomb).is_err() {
        // 既に無い（別の掃除が消した）か、使用中。どちらも任せる。
        return false;
    }
    if expired_at(&tomb) {
        let _ = std::fs::remove_file(&tomb);
        return true;
    }
    // 退避する直前に `put` が新しい中身を置いていた。元へ戻す。
    restore_entry(&tomb, &path);
    false
}

/// 退避してしまった新しい中身を元の名前へ戻す。
///
/// `hard_link` は戻す先が既にあると失敗します。その間に入った `put` の
/// ほうが新しいので、そちらを残して退避したものを捨てます。
/// ハードリンクを作れないファイルシステム（FAT など）では、戻す先が
/// 無いことを確かめてから `rename` します（確かめてから戻すまでの
/// 一瞬に入った `put` は上書きします）。
fn restore_entry(tomb: &Path, path: &Path) {
    match std::fs::hard_link(tomb, path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            if !path.exists() {
                let _ = std::fs::rename(tomb, path);
                return;
            }
        }
    }
    let _ = std::fs::remove_file(tomb);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 中身の形は期限と値に分かれる() {
        let raw = Entry::encode("値", Some(60));
        let entry = Entry::decode(&raw).expect("読める");
        assert_eq!(entry.value, "値");
        assert!(!entry.expired());
        assert!(entry.expires_at > time::now_seconds());

        // 期限なしは 0。
        let raw = Entry::encode("値", None);
        assert!(raw.starts_with("0|"));
        assert!(!Entry::decode(&raw).unwrap().expired());
    }

    #[test]
    fn 改行を含む値も壊れない() {
        let raw = Entry::encode("1行目\n2行目|区切りも入る", Some(10));
        let entry = Entry::decode(&raw).unwrap();
        assert_eq!(entry.value, "1行目\n2行目|区切りも入る");
    }

    #[test]
    fn 壊れた中身はnoneになる() {
        assert!(Entry::decode("区切りなし").is_none());
        assert!(Entry::decode("abc|値").is_none(), "期限が数でない");
    }

    #[test]
    fn 期限切れの判定() {
        let entry = Entry {
            expires_at: time::now_seconds() - 1,
            value: "古い".into(),
        };
        assert!(entry.expired());
    }

    #[test]
    fn メモリの置き場所は出し入れできる() {
        let store = MemoryCache::new();
        store.put("a", "1", Some(60)).unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(store.get("none").unwrap(), None);

        store.forget("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);

        store.put("b", "2", None).unwrap();
        store.flush().unwrap();
        assert_eq!(store.get("b").unwrap(), None);
    }

    #[test]
    fn メモリの置き場所は期限切れを捨てる() {
        let store = MemoryCache::new();
        // 期限を過去にして入れる。
        store
            .entries
            .lock()
            .unwrap()
            .insert("old".into(), format!("{}|値", time::now_seconds() - 10));
        assert_eq!(store.get("old").unwrap(), None, "期限切れは読めない");
        assert!(
            !store.entries.lock().unwrap().contains_key("old"),
            "読んだときに捨てる"
        );
    }

    #[test]
    fn ファイルの置き場所は出し入れできる() {
        let dir = std::env::temp_dir().join(format!("bengara-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileCache::new(&dir);

        store.put("a", "1", Some(60)).unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(store.get("none").unwrap(), None);

        store.forget("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        store.forget("a").unwrap();

        // 期限切れは読めない。ファイルは `get` では消さず、掃除が消す。
        store.put("old", "古い", Some(0)).unwrap();
        assert_eq!(store.get("old").unwrap(), None);
        assert!(dir.join("old").exists(), "`get` は消さない");
        assert_eq!(sweep_expired(&store).unwrap(), 1);
        assert!(!dir.join("old").exists());

        store.put("b", "2", None).unwrap();
        assert_eq!(sweep_expired(&store).unwrap(), 0, "期限なしは消さない");
        store.flush().unwrap();
        assert_eq!(store.get("b").unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限切れの掃除() {
        let dir = std::env::temp_dir().join(format!("bengara-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileCache::new(&dir);

        store.put("keep", "1", Some(600)).unwrap();
        store.put("gone", "2", Some(0)).unwrap();
        std::fs::write(dir.join("broken"), "こわれた中身").unwrap();

        assert_eq!(sweep_expired(&store).unwrap(), 2, "期限切れと壊れたもの");
        assert!(dir.join("keep").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// テスト用の空ディレクトリ。名前が重ならないように連番を足す。
    fn temp_dir(label: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("bengara-{label}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn 拡張子だけ違う鍵は別の一時ファイルを使う() {
        let dir = temp_dir("tmpname");
        let store = FileCache::new(&dir);

        // `with_extension("tmp")` だと両方 `test.tmp` になってしまう。
        let a = store.temp_path_of("test.put");
        let b = store.temp_path_of("test.count");
        assert_ne!(a, b);
        // 印が入っているので、一時ファイルだと分かる。
        assert!(is_temp_path(&a));
        assert!(a.file_name().unwrap().to_str().unwrap().contains(TEMP_MARK));

        // 同じ鍵でも、呼ぶたびに別の名前になる。
        assert_ne!(store.temp_path_of("test.put"), a);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 数字で終わる鍵は一時ファイルと取り違えない() {
        // 「後ろ2つが数字」で見ていたときは、この 2 つを一時ファイルと見なしていた。
        assert!(!is_temp_path(Path::new("report.2024.12.tmp")));
        assert!(!is_temp_path(Path::new("page.1.2.tmp")));
        assert!(!is_temp_path(Path::new("1.2")));
        // 印が入っているものだけが一時ファイル。
        assert!(is_temp_path(Path::new("report.2024.12.+tmp.1234.5")));
        assert!(is_temp_path(Path::new("a.+tmp.1.0")));
    }

    #[test]
    fn 数字で終わる鍵は期限が残っていれば消さない() {
        let dir = temp_dir("digitkey");
        let store = FileCache::new(&dir);

        // 前は `cache:prune` で期限なしでも消えていた。
        store.put("report.2024.12", "1", None).unwrap();
        assert_eq!(sweep_expired(&store).unwrap(), 0);
        assert_eq!(store.get("report.2024.12").unwrap().as_deref(), Some("1"));

        // `cache:clear`（flush）では消える。前は「書き込み中」として飛ばしていた。
        store.flush().unwrap();
        assert_eq!(store.get("report.2024.12").unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 大きな値でも期限だけ見て消す() {
        let dir = temp_dir("prunehead");
        let store = FileCache::new(&dir);

        // 期限は 1 行目の `|` より前にしかない。値の全体は読まない。
        let big = "あ".repeat(200_000);
        store.put("big", &big, Some(0)).unwrap();
        store.put("keep", &big, Some(3_600)).unwrap();
        // 期限だけ読めていることを押さえる。
        let expires = read_expires_at(&store.path_of("keep")).unwrap();
        assert!(expires > time::now_seconds());

        assert_eq!(sweep_expired(&store).unwrap(), 1);
        assert!(!store.path_of("big").exists());
        assert_eq!(store.get("keep").unwrap().as_deref(), Some(big.as_str()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 壊れたファイルは消す() {
        let dir = temp_dir("prunebroken");
        let store = FileCache::new(&dir);

        // `|` が無い・期限が数でない・空。どれも壊れている扱い。
        for (key, raw) in [("a", "|"), ("b", "ごみ"), ("c", "x|値"), ("d", "")] {
            std::fs::write(store.path_of(key), raw).unwrap();
            assert!(read_expires_at(&store.path_of(key)).is_none(), "{key}");
        }
        assert_eq!(sweep_expired(&store).unwrap(), 4);
        // 残るのは錠の置き場所（ディレクトリ）だけ。ファイルも錠も残らない。
        let files = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .count();
        assert_eq!(files, 0);
        let locks = std::fs::read_dir(dir.join(LOCK_DIR))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(locks, 0, "錠は手放したら残らない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn utf8でない中身も壊れたファイルとして消す() {
        let dir = temp_dir("getinvalid");
        let store = FileCache::new(&dir);

        // 期限の部分から UTF-8 でない。`has` は「無い」と答える。
        std::fs::write(store.path_of("bad"), [0xff, 0xfe, b'|', 0xff]).unwrap();
        assert!(!store.has("bad").unwrap(), "`has` は無いと答える");

        // 以前は `Err(Io)` が返り、リクエストが 500 になっていた。
        assert_eq!(store.get("bad").unwrap(), None, "`get` も無いと答える");

        // 値だけが UTF-8 でないものも、読めないので同じ扱い。
        std::fs::write(store.path_of("half"), [b'0', b'|', 0xff, 0xfe]).unwrap();
        assert_eq!(store.get("half").unwrap(), None);

        // 消すのは `get` ではなく掃除。期限の部分が読めない `bad` は壊れた
        // ものとして消す。`half` は期限なし（`0|`）と読めるので残す。
        assert_eq!(sweep_expired(&store).unwrap(), 1);
        assert!(!store.path_of("bad").exists(), "壊れたファイルは掃除が消す");

        // 上書きすれば普通に読める。
        store.put("bad", "値", None).unwrap();
        assert_eq!(store.get("bad").unwrap().as_deref(), Some("値"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 掃除の件数に古い一時ファイルを混ぜない() {
        let dir = temp_dir("prunetemp");
        let store = FileCache::new(&dir);

        store.put("gone", "1", Some(0)).unwrap();
        // 書き込みに失敗した残骸。更新時刻を十分古くする。
        let stale = store.temp_path_of("key");
        std::fs::write(&stale, "残骸").unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(TEMP_STALE_SECS + 60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();

        // 以前は 2 件と数え、「期限切れのキャッシュを 2 件」と出していた。
        assert_eq!(
            sweep_expired(&store).unwrap(),
            1,
            "期限切れの 1 件だけ数える"
        );
        assert!(!stale.exists(), "残骸は消す");
        assert!(!store.path_of("gone").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn storageが無いときの案内はコマンドの呼び方を言う() {
        // `Storage`（`storage.rs`）と同じ文。開発と本番の両方の呼び方を言う。
        // 以前は「デプロイのときに storage/ を作ってください」だけだった。
        let text = crate::paths::missing_storage_message(Path::new("/srv/app/storage"));
        assert!(text.contains("がありません"), "{text}");
        assert!(text.contains("cargo artisan storage:init"), "{text}");
        assert!(text.contains("./<実行ファイル> storage:init"), "{text}");
        assert!(text.ends_with("を実行してください"), "{text}");
    }

    #[test]
    fn 鍵locksは錠の置き場所とぶつからない() {
        let dir = temp_dir("lockskey");
        let store = FileCache::new(&dir);

        // 一度 increment を使って錠の置き場所を作る。
        store.increment("n", 1).unwrap();
        // `normalize_key("locks")` はそのまま `locks` なので、避けようがない。
        store.put("locks", "値", Some(60)).unwrap();
        assert_eq!(store.get("locks").unwrap().as_deref(), Some("値"));

        // 錠の置き場所は `normalize_key` が作れない名前。大文字では駄目（NTFS）。
        assert!(!LOCK_DIR.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(LOCK_DIR.to_ascii_lowercase(), "locks");
        assert!(dir.join(LOCK_DIR).is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 長すぎる期限でもあふれない() {
        // `as i64` だと `u64::MAX` が `-1` になって即時失効していた。
        let raw = Entry::encode("値", Some(u64::MAX));
        let entry = Entry::decode(&raw).unwrap();
        assert_eq!(entry.expires_at, i64::MAX);
        assert!(!entry.expired(), "即時失効しない");

        // i64 の上限に近い値でもパニックしない。
        let entry = Entry::decode(&Entry::encode("値", Some(i64::MAX as u64))).unwrap();
        assert_eq!(entry.expires_at, i64::MAX);
        assert!(!entry.expired());
    }

    #[test]
    fn tmpで終わる鍵も壊れない() {
        let dir = temp_dir("tmpkey");
        let store = FileCache::new(&dir);

        store.put("a.tmp", "1", Some(60)).unwrap();
        assert_eq!(store.get("a.tmp").unwrap().as_deref(), Some("1"));
        // 一時ファイルと見なされないので、本体として掃除の対象になる。
        assert!(!is_temp_path(&dir.join("a.tmp")));
        assert!(is_temp_path(&dir.join("a.tmp.+tmp.1234.5")));

        store.put("b", "2", Some(0)).unwrap();
        assert_eq!(sweep_expired(&store).unwrap(), 1, "期限切れの b だけ");
        assert!(dir.join("a.tmp").exists());

        store.flush().unwrap();
        assert_eq!(store.get("a.tmp").unwrap(), None, "flush で消える");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 掃除は新しい一時ファイルを残す() {
        let dir = temp_dir("tmpsweep");
        let store = FileCache::new(&dir);

        // 書き込み途中に見える一時ファイル。
        let writing = store.temp_path_of("key");
        std::fs::write(&writing, "まだ書いている途中").unwrap();

        assert_eq!(
            sweep_expired(&store).unwrap(),
            0,
            "新しい一時ファイルは残す"
        );
        assert!(writing.exists());

        store.flush().unwrap();
        assert!(writing.exists(), "flush でも消さない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 数を足せる() {
        let dir = temp_dir("incr");
        let store = FileCache::new(&dir);

        assert_eq!(store.increment("n", 1).unwrap(), 1);
        assert_eq!(store.increment("n", 2).unwrap(), 3);
        assert_eq!(store.increment("n", -1).unwrap(), 2);
        assert_eq!(store.get("n").unwrap().as_deref(), Some("2"));
        // 錠は手放したら残らない。
        assert!(!store.lock_path_of("n").exists());

        // 数でない値はエラーにする。
        store.put("word", "あ", None).unwrap();
        assert!(store.increment("word", 1).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn メモリの置き場所も数を足せる() {
        let store = MemoryCache::new();
        assert_eq!(store.increment("n", 1).unwrap(), 1);
        assert_eq!(store.increment("n", 2).unwrap(), 3);
        assert_eq!(store.increment("n", -4).unwrap(), -1);

        store.put("word", "あ", None).unwrap();
        assert!(store.increment("word", 1).is_err());

        // 期限切れは 0 から数え直す。
        store.put("old", "5", Some(0)).unwrap();
        assert_eq!(store.increment("old", 1).unwrap(), 1);
    }

    #[test]
    fn 数を足してもあふれない() {
        let dir = temp_dir("incrsat");
        let store = FileCache::new(&dir);

        // `current + by` だと、デバッグビルドでは裏のスレッドが落ちて 500 に
        // なり、リリースでは `i64::MIN` に巻き戻っていた。
        store.put("max", &i64::MAX.to_string(), None).unwrap();
        assert_eq!(store.increment("max", 1).unwrap(), i64::MAX);
        assert_eq!(store.increment("max", i64::MAX).unwrap(), i64::MAX);

        // 下側も止まる（`Cache::decrement` が通る道）。
        store.put("min", &i64::MIN.to_string(), None).unwrap();
        assert_eq!(store.increment("min", -1).unwrap(), i64::MIN);
        assert_eq!(store.increment("min", i64::MIN).unwrap(), i64::MIN);

        // メモリの置き場所も同じ。
        let memory = MemoryCache::new();
        memory.put("max", &i64::MAX.to_string(), None).unwrap();
        assert_eq!(memory.increment("max", 1).unwrap(), i64::MAX);
        memory.put("min", &i64::MIN.to_string(), None).unwrap();
        assert_eq!(memory.increment("min", i64::MIN).unwrap(), i64::MIN);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn あるかは中身を読まない() {
        let dir = temp_dir("hashead");
        let store = FileCache::new(&dir);

        // `put_json` で入れた大きな値でも、期限だけ見て答える。
        let big = "あ".repeat(200_000);
        store.put("big", &big, Some(3_600)).unwrap();
        assert!(store.has("big").unwrap());

        // 期限なしも真。無いものと壊れたものは偽。
        store.put("forever", "1", None).unwrap();
        assert!(store.has("forever").unwrap());
        assert!(!store.has("none").unwrap());
        std::fs::write(store.path_of("broken"), "ごみ").unwrap();
        assert!(!store.has("broken").unwrap());

        // 期限切れは偽。`has` も `get` も消さない（掃除に任せる）。
        store.put("old", &big, Some(0)).unwrap();
        assert!(!store.has("old").unwrap());
        assert!(store.path_of("old").exists(), "`has` は消さない");
        assert_eq!(store.get("old").unwrap(), None);
        assert!(store.path_of("old").exists(), "`get` も消さない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限は先頭だけ読む() {
        let dir = temp_dir("head32");
        let store = FileCache::new(&dir);

        // `i64` は符号を入れて 20 文字まで。`|` は 21 バイト目までに必ず来る。
        assert!(i64::MIN.to_string().len() <= 20);
        std::fs::write(store.path_of("max"), format!("{}|値", i64::MAX)).unwrap();
        assert_eq!(read_expires_at(&store.path_of("max")), Some(i64::MAX));

        // 先頭 32 バイトに `|` が無ければ、壊れているものとして扱う。
        std::fs::write(store.path_of("far"), format!("{}|値", "9".repeat(40))).unwrap();
        assert_eq!(read_expires_at(&store.path_of("far")), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ブロックするかを言える() {
        assert!(
            FileCache::new("どこか").blocking(),
            "ファイルはブロックする"
        );
        assert!(!MemoryCache::new().blocking());
    }

    #[test]
    fn 掃除ができるかを言える() {
        let store = FileCache::new("どこか");
        assert!(store.prunable());
        assert_eq!(store.location().as_deref(), Some("どこか"));

        let memory = MemoryCache::new();
        assert!(!memory.prunable());
        assert_eq!(memory.prune().unwrap(), 0);
        assert_eq!(memory.location(), None);
    }

    #[test]
    fn あるかを調べられる() {
        let store = MemoryCache::new();
        store.put("a", "1", Some(60)).unwrap();
        assert!(store.has("a").unwrap());
        assert!(!store.has("b").unwrap());

        let dir = temp_dir("has");
        let file = FileCache::new(&dir);
        file.put("a", "1", Some(60)).unwrap();
        assert!(file.has("a").unwrap());
        assert!(!file.has("b").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 掃除は置き換わった新しい値を消さない() {
        let dir = temp_dir("prunerace");
        let store = FileCache::new(&dir);

        // 候補に選んだ後で `put` が新しい値に置き換えた形。見直して消さない。
        store.put("k", "新しい", Some(600)).unwrap();
        assert!(!remove_if_expired(&store, "k"));
        assert_eq!(store.get("k").unwrap().as_deref(), Some("新しい"));

        // 退避した後で新しいと分かったら戻す。戻す先が空いていれば戻る。
        let tomb = store.temp_path_of("k");
        std::fs::rename(store.path_of("k"), &tomb).unwrap();
        restore_entry(&tomb, &store.path_of("k"));
        assert_eq!(store.get("k").unwrap().as_deref(), Some("新しい"));
        assert!(!tomb.exists(), "退避先は残さない");

        // 戻すまでの間にさらに `put` があれば、そちらを残す（上書きしない）。
        let tomb = store.temp_path_of("k");
        std::fs::rename(store.path_of("k"), &tomb).unwrap();
        store.put("k", "もっと新しい", Some(600)).unwrap();
        restore_entry(&tomb, &store.path_of("k"));
        assert_eq!(store.get("k").unwrap().as_deref(), Some("もっと新しい"));
        assert!(!tomb.exists(), "退避先は残さない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 数えている最中の鍵は掃除しない() {
        let dir = temp_dir("prunelock");
        let store = FileCache::new(&dir);

        store.put("n", "5", Some(0)).unwrap();
        // `increment` が錠を握っている間は、期限切れに見えても消さない。
        // 消すと、書き直したばかりの数を消して落とすことがある。
        let held = FileLock::acquire(&store.lock_path_of("n")).unwrap();
        assert_eq!(sweep_expired(&store).unwrap(), 0);
        assert!(store.path_of("n").exists());
        drop(held);

        assert_eq!(sweep_expired(&store).unwrap(), 1);
        assert!(!store.path_of("n").exists());
        // 期限切れからは 0 で数え直す（今までどおり）。
        store.put("n", "5", Some(0)).unwrap();
        assert_eq!(store.increment("n", 1).unwrap(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無いディレクトリの掃除は0件() {
        assert_eq!(
            sweep_expired(&FileCache::new("存在しないディレクトリ")).unwrap(),
            0
        );
    }

    #[test]
    fn メモリの置き場所は期限切れをまとめて落とす() {
        let store = MemoryCache::new();
        let every = MEMORY_GC_EVERY;
        for i in 0..every {
            // 秒 0 はその場で期限切れ。読まなければ残り続けていた。
            store.put(&format!("k{i}"), "v", Some(0)).unwrap();
        }
        store.put("keep", "v", None).unwrap();
        let len = store.lock().len();
        assert!(len <= 2, "期限切れが残らない: {len}");
        assert_eq!(store.get("keep").unwrap().as_deref(), Some("v"));
    }
}
