//! ジョブと聞く側が受け取る中身の形。
//!
//! 同じ型を `bengara::queue` と `bengara::events` の 2 か所から出します。
//! 直下（`bengara::Json`）には出しません。`bengara::Error::Json` と紛らわしいためです。

use crate::error::{Error, Result};

/// JSON の中身を型に読んで受け取る。
///
/// ```ignore
/// // app/Jobs/SendWelcome.rs
/// use bengara::queue::Json;
///
/// #[derive(bengara::serde::Deserialize)]
/// #[serde(crate = "bengara::serde")]
/// pub struct Payload {
///     pub user_id: i64,
/// }
///
/// pub async fn handle(Json(payload): Json<Payload>) -> Result<()> {
///     // payload.user_id を使う
///     Ok(())
/// }
/// ```
///
/// 読めないときは `Err` です。ジョブなら失敗として再挑戦と `failed_jobs` に乗り、
/// 聞く側なら聞く側の失敗になります。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json<T>(pub T);

/// ジョブと聞く側が受け取れる形。`String` と [`Json<T>`] があります。
///
/// 生成コードはハンドラの引数の型から、どちらで読むかを決めます。
pub trait FromPayload: Sized {
    /// 中身（文字列）から作る。
    fn from_payload(raw: String) -> Result<Self>;
}

/// そのまま渡す。今までの `handle(payload: String)` の形です。
impl FromPayload for String {
    fn from_payload(raw: String) -> Result<Self> {
        Ok(raw)
    }
}

/// `serde_json` で読む。
impl<T: serde::de::DeserializeOwned> FromPayload for Json<T> {
    fn from_payload(raw: String) -> Result<Self> {
        serde_json::from_str(&raw).map(Json).map_err(|e| {
            Error::msg(format!(
                "ジョブ・聞く側の中身を `{}` として読めません: {e}",
                std::any::type_name::<T>()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Payload {
        id: i64,
    }

    #[test]
    fn 文字列はそのまま渡す() {
        assert_eq!(String::from_payload("{壊れた".into()).unwrap(), "{壊れた");
    }

    #[test]
    fn jsonは型に読む() {
        let Json(p) = Json::<Payload>::from_payload(r#"{"id":7}"#.into()).unwrap();
        assert_eq!(p, Payload { id: 7 });
    }

    #[test]
    fn 読めないjsonは型名を添えたエラーになる() {
        let error = Json::<Payload>::from_payload("{壊れた".into())
            .unwrap_err()
            .to_string();
        assert!(error.contains("ジョブ・聞く側の中身を"), "{error}");
        assert!(error.contains("Payload"), "型名が出る: {error}");
    }
}
