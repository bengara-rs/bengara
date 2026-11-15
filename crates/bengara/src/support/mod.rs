//! 小さな下支え。利用者には見せません。

pub(crate) mod crypto;
pub(crate) mod fs;
pub(crate) mod lock;
pub(crate) mod text;
pub(crate) mod time;

use crate::error::{Error, Result};

/// `MemoryStore` / `MemoryCache` が、書き込みの何回に 1 回、期限切れをまとめて落とすか。
///
/// 読んだ鍵しか消さない作りだと、二度と読まれない鍵が増え続けます。
/// セッションとキャッシュの両方が使うので、どちらの持ち物でもないここに置きます。
pub(crate) const MEMORY_GC_EVERY: u64 = 100;

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
