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
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(path, LOCK_STALE_SECS) {
                        // 錠を取ったプロセスが落ちたと見なして外す。
                        tracing::warn!("古い錠 {} を外します", path.display());
                        remove_stale(path);
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
fn remove_stale(path: &Path) {
    let seq = STALE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let moved = path.with_extension(format!("stale.{}.{seq}", std::process::id()));
    if std::fs::rename(path, &moved).is_ok() {
        let _ = std::fs::remove_file(&moved);
    }
}

/// 退避先の名前が重ならないようにする連番。
static STALE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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
        std::fs::write(&path, "").unwrap();
        remove_stale(&path);
        assert!(!path.exists(), "古い錠は外れる");

        // 退避先（`*.stale.*`）が残っていない。
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "残っている: {left:?}");

        // 既に無いときは何もしない（負けた側はここを通る）。
        remove_stale(&path);
        assert!(!path.exists());

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
