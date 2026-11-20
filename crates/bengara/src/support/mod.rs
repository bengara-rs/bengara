//! 小さな下支え。Laravel の `Illuminate\Support` に当たります。
//!
//! 利用者に見せるのは [`blocking`] と [`Carbon`] だけです。ほかは枠組みの中で使います。

mod carbon;
pub(crate) mod crypto;
pub(crate) mod fs;
pub(crate) mod lock;
pub(crate) mod payload;
pub(crate) mod text;
pub(crate) mod time;

pub use carbon::Carbon;

use crate::error::{Error, Result};

/// `MemoryStore` / `MemoryCache` が、書き込みの何回に 1 回、期限切れをまとめて落とすか。
///
/// 読んだ鍵しか消さない作りだと、二度と読まれない鍵が増え続けます。
/// セッションとキャッシュの両方が使うので、どちらの持ち物でもないここに置きます。
pub(crate) const MEMORY_GC_EVERY: u64 = 100;

/// 時間のかかる同期の処理を、裏のスレッドで行って結果を待つ。
///
/// ファイルの読み書き・ハッシュの計算・画像の変換のように、`await` を挟まずに
/// しばらく CPU やディスクを使う処理に使います。ハンドラやジョブの中で
/// そのまま呼ぶと、同じスレッドで動いているほかのリクエストが止まります。
///
/// ```ignore
/// use bengara::support::blocking;
///
/// let size = blocking(move || std::fs::metadata(&path).map(|m| m.len())).await??;
/// ```
///
/// | 起きたこと | 結果 |
/// |---|---|
/// | `f` が値を返した | `Ok(値)` |
/// | `f` がパニックした | `Err`（500）。パニックはその処理に閉じ込めます |
///
/// **途中で止められません。** 呼んだ側が待つのをやめても（打ち切り・切断）、
/// `f` は最後まで走ります。外部のコマンドを待つときは、打ち切ると止まる
/// [`Process`](crate::process::Process) を使ってください。
///
/// 裏のスレッドは tokio の待ち用のスレッド（既定で最大 512 本）です。
/// 何十分も占有する処理を大量に並べると、枠組みの読み書き（セッション・
/// キャッシュ・ファイル）まで待たされます。
pub async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::msg(format!("裏のスレッドでの処理に失敗しました: {e}")))
}
