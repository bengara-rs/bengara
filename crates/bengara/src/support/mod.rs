//! 小さな下支え。利用者には見せません。

pub(crate) mod crypto;
pub(crate) mod lock;
pub(crate) mod text;
pub(crate) mod time;

use crate::error::{Error, Result};

/// 時間のかかる処理（ファイルの読み書き、ハッシュの計算）を裏のスレッドで行う。
///
/// そのまま待つと、同じスレッドで動いているほかのリクエストが止まります。
pub(crate) async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::msg(format!("裏のスレッドでの処理に失敗しました: {e}")))
}
