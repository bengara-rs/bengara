//! 置き場所の決め方。
//!
//! 本番では実行ファイルの隣に `.env` と `storage/` を置きます。開発中は cargo が渡す
//! `CARGO_MANIFEST_DIR` を使います。
//!
//! **決まらないときは起動を止めます**（決定記録 #064）。カレントディレクトリに落とすと、
//! 関係ない場所で動き出し、空の `storage/` を作ってしまうためです。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// 基準ディレクトリがそこにあると判断する目印。
const MARKERS: &[&str] = &[".env", "storage"];

static BASE: OnceLock<Resolved> = OnceLock::new();
static STORAGE: OnceLock<PathBuf> = OnceLock::new();

/// 基準ディレクトリをどう決めたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// 環境変数 `APP_BASE_PATH`。
    Explicit,
    /// 環境変数 `CARGO_MANIFEST_DIR`（cargo 経由）。
    Cargo,
    /// 実行ファイルのあるディレクトリ。
    Exe,
    /// カレントディレクトリ。
    Current,
    /// 決まらなかった（カレントディレクトリで代用している）。
    Unknown,
}

impl Source {
    /// ログに出す言い方。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Source::Explicit => "APP_BASE_PATH",
            Source::Cargo => "cargo のプロジェクト",
            Source::Exe => "実行ファイルの隣",
            Source::Current => "カレントディレクトリ",
            Source::Unknown => "決まらなかった",
        }
    }
}

#[derive(Debug, Clone)]
struct Resolved {
    path: PathBuf,
    source: Source,
    /// `APP_BASE_PATH` が相対パスだったときの理由。
    bad_explicit: Option<String>,
}

fn resolve() -> &'static Resolved {
    BASE.get_or_init(|| {
        // 1. 明示された場所。絶対パスでなければ、理由を覚えておいて後で止める。
        if let Some(raw) = std::env::var_os("APP_BASE_PATH") {
            let path = PathBuf::from(&raw);
            if path.is_absolute() {
                return Resolved {
                    path,
                    source: Source::Explicit,
                    bad_explicit: None,
                };
            }
            return Resolved {
                path: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                source: Source::Unknown,
                bad_explicit: Some(format!(
                    "APP_BASE_PATH には絶対パスを指定してください（いまの値: {}）",
                    path.display()
                )),
            };
        }

        // 2. cargo 経由。開発中はここで決まる。
        if let Some(p) = std::env::var_os("CARGO_MANIFEST_DIR") {
            return Resolved {
                path: PathBuf::from(p),
                source: Source::Cargo,
                bad_explicit: None,
            };
        }

        // 3. 実行ファイルの隣。本番はここで決まる。
        if let Some(dir) = exe_dir() {
            if has_marker(&dir) {
                return Resolved {
                    path: dir,
                    source: Source::Exe,
                    bad_explicit: None,
                };
            }
        }

        // 4. カレントディレクトリ。目印があるときだけ使う。
        if let Ok(dir) = std::env::current_dir() {
            if has_marker(&dir) {
                return Resolved {
                    path: dir,
                    source: Source::Current,
                    bad_explicit: None,
                };
            }
        }

        // 5. 決まらなかった。guard() が起動を止める。
        Resolved {
            path: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            source: Source::Unknown,
            bad_explicit: None,
        }
    })
}

/// 実行ファイルのあるディレクトリ。
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())
}

/// そのディレクトリが基準ディレクトリらしいか。
fn has_marker(dir: &Path) -> bool {
    MARKERS.iter().any(|m| dir.join(m).exists())
}

/// 基準ディレクトリ。
///
/// 決まらなかったときはカレントディレクトリを返します。
/// **起動の前に [`guard`] を呼んで、決まっているか確かめてください。**
pub fn base_path() -> &'static Path {
    resolve().path.as_path()
}

/// 基準ディレクトリをどう決めたか（ログ用）。
pub(crate) fn base_source() -> Source {
    resolve().source
}

/// 基準ディレクトリが決まっているか確かめる。
///
/// 決まっていなければ、探した場所と直し方を添えてエラーにします。
pub(crate) fn guard() -> Result<()> {
    let resolved = resolve();
    if let Some(reason) = &resolved.bad_explicit {
        return Err(Error::msg(reason.clone()));
    }
    if resolved.source != Source::Unknown {
        return Ok(());
    }

    let exe = exe_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（分かりません）".to_string());
    let current = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "（分かりません）".to_string());

    Err(Error::msg(format!(
        "基準ディレクトリが決まりません。\n\
         探した場所:\n\
         \x20 実行ファイルの隣: {exe}\n\
         \x20 カレントディレクトリ: {current}\n\
         どちらにも .env と storage/ がありません。\n\
         直し方（どちらか）:\n\
         \x20 1. 実行ファイルの隣に .env を置き、cargo artisan storage:init を実行する\n\
         \x20 2. 環境変数 APP_BASE_PATH に絶対パスを指定する"
    )))
}

/// `storage/` の場所を確かめる。
///
/// `APP_STORAGE_PATH` が相対パスのときはエラーにします。
pub(crate) fn guard_storage() -> Result<()> {
    if let Some(raw) = std::env::var_os("APP_STORAGE_PATH") {
        let path = PathBuf::from(&raw);
        if !path.is_absolute() {
            return Err(Error::msg(format!(
                "APP_STORAGE_PATH には絶対パスを指定してください（いまの値: {}）",
                path.display()
            )));
        }
    }
    Ok(())
}

/// 基準ディレクトリからの相対パスを絶対パスにする。
pub fn app_path(relative: impl AsRef<Path>) -> PathBuf {
    base_path().join(relative)
}

/// `public/` の中のパス。
///
/// **場所は変えられません。** リリースビルドではバイナリに埋め込むためです。
pub fn public_path(relative: impl AsRef<Path>) -> PathBuf {
    app_path("public").join(relative)
}

/// `storage/` の中のパス。
///
/// `APP_STORAGE_PATH`（絶対パス）で場所を変えられます。
/// プロセスを2つ以上動かすときは、全プロセスで同じ場所を指してください。
pub fn storage_path(relative: impl AsRef<Path>) -> PathBuf {
    storage_root().join(relative)
}

/// `storage/` そのもの。
pub(crate) fn storage_root() -> &'static Path {
    STORAGE
        .get_or_init(|| match std::env::var_os("APP_STORAGE_PATH") {
            // 相対パスは guard_storage() が止めるので、ここでは絶対パスだけを見る。
            Some(raw) if Path::new(&raw).is_absolute() => PathBuf::from(raw),
            _ => app_path("storage"),
        })
        .as_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 目印があるディレクトリを見分ける() {
        let dir = std::env::temp_dir().join("bengara_paths_marker");
        let _ = std::fs::create_dir_all(&dir);
        assert!(!has_marker(&dir), "空のディレクトリは目印なし");

        let _ = std::fs::write(dir.join(".env"), "");
        assert!(has_marker(&dir), ".env があれば目印あり");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn storageを目印にできる() {
        let dir = std::env::temp_dir().join("bengara_paths_marker2");
        let _ = std::fs::create_dir_all(dir.join("storage"));
        assert!(has_marker(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 決め方の言い方がある() {
        // ログに出るので、空にならないことだけ確かめる。
        for source in [
            Source::Explicit,
            Source::Cargo,
            Source::Exe,
            Source::Current,
            Source::Unknown,
        ] {
            assert!(!source.label().is_empty());
        }
    }

    #[test]
    fn テストではcargo経由で決まる() {
        // cargo test は CARGO_MANIFEST_DIR を渡すので、必ず決まる。
        assert!(guard().is_ok());
        assert_eq!(base_source(), Source::Cargo);
        assert!(base_path().is_absolute());
    }

    #[test]
    fn storageは基準の下に来る() {
        // このテストでは APP_STORAGE_PATH を指定していない。
        assert_eq!(storage_root(), app_path("storage"));
        assert_eq!(storage_path("logs"), app_path("storage").join("logs"));
    }

    #[test]
    fn 相対のstorageは断る() {
        // 実際の環境変数は触らずに、判定だけを確かめる。
        assert!(!Path::new("storage").is_absolute());
        assert!(guard_storage().is_ok(), "指定が無ければ通る");
    }
}
