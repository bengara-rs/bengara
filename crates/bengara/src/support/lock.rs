//! ファイルで取る錠。プロセスをまたいで1つだけ通します。
//!
//! `create_new(true)` で作れた側だけが進めます。依存クレートを増やさずに、
//! どの OS でも同じように動く排他にするためです（同じプロセスのスレッド同士にも効きます）。
//!
//! キャッシュの `increment`（`cache/store.rs`）と、`schedule:run`
//! （`schedule.rs`）の両方から使います。

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 錠ファイルがこの秒数より古ければ、置いたプロセスが落ちたと見なして外す。
const LOCK_STALE_SECS: u64 = 10;

/// 錠を取るのを諦めるまでの回数と、1回あたりの待ち時間（ミリ秒）。
///
/// **合計の待ち時間は [`LOCK_STALE_SECS`] より長くします。** 短いと、錠を
/// 置いたプロセスが落ちた直後に来た側が、古い錠と見なされる前に諦めます。
/// 1200 × 10ms = 12 秒で、10 秒より 2 秒長いので必ず外す側まで待てます。
/// 回数制限（throttle）は `Cache::increment` を使うので、ここで諦めると
/// 落ちた 1 プロセスのせいで同じ鍵への要求が失敗し続けます。
const LOCK_TRIES: u32 = 1200;
const LOCK_SLEEP_MS: u64 = 10;

/// 1つの名前を守る錠ファイル。**手放すと消えます。**
pub(crate) struct FileLock {
    path: PathBuf,
    /// 錠ファイルに書いた持ち主の印。
    ///
    /// **`Drop` はこれが一致したときだけ消します。** 印が無かったころは
    /// パスしか持たず、そこにある**誰の**錠でも消していました。錠ファイルの
    /// mtime は作られたときのままなので、10 秒より長く持ち続けた持ち主は
    /// 「死んだ」と判定されて錠を奪われます。奪われた側の `Drop` が
    /// 新しい持ち主の錠を消し、さらに別の側が錠を取れてしまいました。
    stamp: String,
}

/// 持ち主の印を作る。プロセス番号・連番・時刻を並べます。
///
/// `support::crypto::random_token` は使いません。`support` の中で依存の
/// 向きを増やさないためです。別のプロセス・別のスレッド・同じ場所の
/// 取り直しで重ならなければ足ります。
fn new_stamp() -> String {
    let seq = STALE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{seq}-{nanos}", std::process::id())
}

/// 錠ファイルに書かれている印を読む。読めなければ `None`。
fn read_stamp(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|raw| raw.trim().to_string())
}

impl FileLock {
    /// 錠を取る。取れなければ少し待って試し直し、諦めたらエラーにします。
    ///
    /// 古い錠（[`LOCK_STALE_SECS`] 秒より前のもの）は、置いたプロセスが
    /// 落ちたと見なして外します。残ったままだと永久に取れなくなるためです。
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        Self::acquire_tries(path, LOCK_TRIES)
    }

    /// 試す回数を決めて錠を取る（テストから使います）。
    pub(crate) fn acquire_tries(path: &Path, tries: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        for _ in 0..tries {
            let stamp = new_stamp();
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    // 印は `create_new` で作れたハンドルにそのまま書く。
                    // 開き直すと、その間に奪われた錠へ書き足してしまう。
                    use std::io::Write;
                    file.write_all(stamp.as_bytes())?;
                    file.flush()?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                        stamp,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(path, LOCK_STALE_SECS) {
                        // 錠を取ったプロセスが落ちたと見なして外す。
                        tracing::warn!("古い錠 {} を外します", path.display());
                        remove_stale(path, read_stamp(path).as_deref());
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(LOCK_SLEEP_MS));
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Err(Error::msg(format!(
            "錠 {} を取れませんでした",
            path.display()
        )))
    }
}

/// 古い錠を外す。**別名へ退避できた側だけが片付けます。**
///
/// `remove_file` で直に消すと、「古いか調べる」と「消す」が別の操作なので、
/// A が消して錠を取った直後に B がその**生きている錠を消して**しまいます。
/// すると A と B の両方が錠を持ち、A の `Drop` が B の錠を消します。
/// `rename` は片方しか成功しない（負けた側は `NotFound`）ので、成功した側だけが
/// 退避先を消します。負けた側はそのまま `create_new` を試し直すだけです。
///
/// `rename` が解けるのは「奪う側どうし」の競合だけです。そこで**印が
/// `expected` と同じときだけ奪います。** 調べてから奪うまでの間に別の側が
/// 取り直していたら、退避したものを戻して何もしません。
fn remove_stale(path: &Path, expected: Option<&str>) {
    // 奪う直前にもう一度読む。調べたときと印が違えば、別の持ち主の
    // 生きた錠なので触らない。
    if read_stamp(path).as_deref() != expected {
        return;
    }
    let seq = STALE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let moved = path.with_extension(format!("stale.{}.{seq}", std::process::id()));
    if std::fs::rename(path, &moved).is_err() {
        return;
    }
    // 退避したものの印を見る。違っていれば、奪う直前に取り直された
    // 生きた錠を動かしてしまったので元に戻す。
    let moved_stamp = read_stamp(&moved);
    if moved_stamp.as_deref() != expected {
        if let Some(stamp) = &moved_stamp {
            restore(path, stamp);
        }
        tracing::warn!("{} は生きた錠だったので戻します", path.display());
    }
    let _ = std::fs::remove_file(&moved);
}

/// 退避してしまった錠を元の場所へ戻す。
///
/// `create_new` なので、すでに別の側が錠を取っていたら何もしません。
/// その側の錠は印が違うので、戻せなかった持ち主の `Drop` には消されません。
fn restore(path: &Path, stamp: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        let _ = file.write_all(stamp.as_bytes());
    }
}

/// 退避先の名前と持ち主の印が重ならないようにする連番。
static STALE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Drop for FileLock {
    /// **自分の印が書かれているときだけ消します。**
    ///
    /// パスだけを見て消していたころは、古いと判定されて奪われた後の
    /// 錠（別の持ち主のもの）まで消していました。
    fn drop(&mut self) {
        if read_stamp(&self.path).as_deref() == Some(self.stamp.as_str()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// 最後に書かれてから `secs` 秒以上たっているか。分からないときは偽。
pub(crate) fn is_stale(path: &Path, secs: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age.as_secs() >= secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bengara-lock-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn 錠は1つだけ通る() {
        let dir = temp_dir("one");
        let path = dir.join("a.lock");

        let held = FileLock::acquire(&path).unwrap();
        assert!(path.exists());
        // 取れないまま諦める（古い錠ではないので外さない）。
        // 既定の回数だと 12 秒待つので、テストでは回数を減らす。
        assert!(FileLock::acquire_tries(&path, 2).is_err());

        drop(held);
        assert!(!path.exists(), "手放すと消える");
        // 空いたので取れる。
        let _again = FileLock::acquire(&path).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 親のディレクトリは作る() {
        let dir = temp_dir("parent");
        let path = dir.join("中").join("a.lock");
        let _held = FileLock::acquire(&path).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 古い錠は外れて退避先も残らない() {
        let dir = temp_dir("takeover");
        let path = dir.join("a.lock");
        // 落ちたプロセスが置いたままの錠。`LOCK_STALE_SECS` より古い扱いにしたいが、
        // 時刻を細工しないので「0 秒で古い」と同じ条件を作れない。
        // ここでは退避の仕組みそのものを確かめる。
        std::fs::write(&path, "だれかの印").unwrap();
        remove_stale(&path, Some("だれかの印"));
        assert!(!path.exists(), "古い錠は外れる");

        // 退避先（`*.stale.*`）が残っていない。
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "残っている: {left:?}");

        // 既に無いときは何もしない（負けた側はここを通る）。
        remove_stale(&path, None);
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 印が違う錠は奪わない() {
        let dir = temp_dir("stamp-keep");
        let path = dir.join("a.lock");
        // 「古い」と判定したときに見た印と、いま書かれている印が違う。
        // ＝その間に別の側が取り直した生きた錠なので触らない。
        std::fs::write(&path, "新しい持ち主").unwrap();
        remove_stale(&path, Some("見たときの印"));
        assert!(path.exists(), "生きた錠は残る");
        assert_eq!(read_stamp(&path).as_deref(), Some("新しい持ち主"));

        // 退避先（`*.stale.*`）も残らない。
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "a.lock")
            .collect();
        assert!(left.is_empty(), "残っている: {left:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 錠には持ち主の印が入る() {
        let dir = temp_dir("stamp");
        let path = dir.join("a.lock");

        let held = FileLock::acquire(&path).unwrap();
        let stamp = read_stamp(&path).expect("印が書かれている");
        assert!(!stamp.is_empty());
        assert!(
            stamp.starts_with(&format!("{}-", std::process::id())),
            "{stamp}"
        );
        drop(held);

        // 取り直すと別の印になる。
        let _again = FileLock::acquire(&path).unwrap();
        assert_ne!(read_stamp(&path).as_deref(), Some(stamp.as_str()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 手放すときに他人の錠は消さない() {
        let dir = temp_dir("not-mine");
        let path = dir.join("a.lock");

        let held = FileLock::acquire(&path).unwrap();
        // 古いと判定されて奪われ、別の持ち主が錠を置いた形。
        std::fs::write(&path, "別の持ち主").unwrap();

        // 印を見ないころは、ここで別の持ち主の錠まで消していた。
        drop(held);
        assert!(path.exists(), "他人の錠は残る");
        assert_eq!(read_stamp(&path).as_deref(), Some("別の持ち主"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 待ち時間の合計は古いと見なす時間より長い() {
        // 短いと、落ちたプロセスの錠が古いと見なされる前に諦めてしまう。
        let waits_ms = u64::from(LOCK_TRIES) * LOCK_SLEEP_MS;
        assert!(
            waits_ms > LOCK_STALE_SECS * 1000,
            "待ち {waits_ms}ms ≦ 古い扱い {}ms",
            LOCK_STALE_SECS * 1000
        );
    }

    #[test]
    fn 新しいファイルは古くない() {
        let dir = temp_dir("stale");
        let path = dir.join("a");
        std::fs::write(&path, "").unwrap();

        assert!(!is_stale(&path, 10));
        // 0 秒なら必ず古い扱いになる。
        assert!(is_stale(&path, 0));
        // 無いファイルは分からないので偽。
        assert!(!is_stale(&dir.join("none"), 0));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
