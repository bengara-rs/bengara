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
const LOCK_TRIES: u32 = 100;
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        for _ in 0..LOCK_TRIES {
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
                        let _ = std::fs::remove_file(path);
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
        assert!(FileLock::acquire(&path).is_err());

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
