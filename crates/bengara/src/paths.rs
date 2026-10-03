//! 基準ディレクトリの決め方。
//!
//! 本番ではバイナリの隣に `.env` と `storage/` を置きます。開発中は cargo が渡す
//! `CARGO_MANIFEST_DIR` を使います。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static BASE: OnceLock<PathBuf> = OnceLock::new();

/// 基準ディレクトリ。次の順で決めます。
///
/// 1. 環境変数 `APP_BASE_PATH`
/// 2. 環境変数 `CARGO_MANIFEST_DIR`（cargo 経由で実行したとき）
/// 3. カレントディレクトリ
pub fn base_path() -> &'static Path {
    BASE.get_or_init(|| {
        if let Some(p) = std::env::var_os("APP_BASE_PATH") {
            return PathBuf::from(p);
        }
        if let Some(p) = std::env::var_os("CARGO_MANIFEST_DIR") {
            return PathBuf::from(p);
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    })
    .as_path()
}

/// 基準ディレクトリからの相対パスを絶対パスにする。
pub fn app_path(relative: impl AsRef<Path>) -> PathBuf {
    base_path().join(relative)
}

/// `public/` の中のパス。
pub fn public_path(relative: impl AsRef<Path>) -> PathBuf {
    app_path("public").join(relative)
}

/// `storage/` の中のパス。
pub fn storage_path(relative: impl AsRef<Path>) -> PathBuf {
    app_path("storage").join(relative)
}
