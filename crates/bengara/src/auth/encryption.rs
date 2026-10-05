//! 暗号化（機能フラグ `encryption`）。
//!
//! ChaCha20-Poly1305（RFC 8439）を `chacha20poly1305` クレートで使います。
//!
//! **ここだけが暗号のクレートを知っています。** 上の層には文字列しか渡しません。
//! 署名（SHA-256 / HMAC）は自前で持っていますが、暗号化は借ります。
//! 署名は間違えればテストで落ちますが、暗号化は間違っていても動いてしまい、
//! 弱いまま運用に乗る危険があるためです（決定記録 #050）。

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use crate::error::{Error, Result};
use crate::support::crypto;

/// 使い捨ての値（nonce）の長さ。
const NONCE_BYTES: usize = 12;

/// `APP_KEY` から 32 バイトの鍵を作る。
fn cipher() -> Result<ChaCha20Poly1305> {
    let app_key = crate::config_registry::app_config().signing_key()?;
    Ok(cipher_from(app_key))
}

/// 任意の鍵から作る（テストで固定の鍵を使うため）。
fn cipher_from(app_key: &[u8]) -> ChaCha20Poly1305 {
    let key_bytes = crypto::sha256(app_key);
    let key = Key::from_slice(&key_bytes);
    ChaCha20Poly1305::new(key)
}

/// 暗号化する。**同じ平文でも毎回違う結果**になります。
pub(crate) fn encrypt(plain: &str) -> Result<String> {
    encrypt_with(&cipher()?, plain)
}

fn encrypt_with(cipher: &ChaCha20Poly1305, plain: &str) -> Result<String> {
    let nonce_bytes = crypto::random_bytes::<NONCE_BYTES>();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let encrypted = cipher
        .encrypt(nonce, plain.as_bytes())
        .map_err(|_| Error::msg("暗号化に失敗しました"))?;

    let mut out = Vec::with_capacity(NONCE_BYTES + encrypted.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&encrypted);
    Ok(crypto::to_hex(&out))
}

/// 元に戻す。改ざんされていればエラー。
///
/// **形が壊れている場合と、改ざんされている場合を区別しません。**
/// どちらなのかが分かると、総当たりの手がかりになります。
pub(crate) fn decrypt(cipher_text: &str) -> Result<String> {
    decrypt_with(&cipher()?, cipher_text)
}

fn decrypt_with(cipher: &ChaCha20Poly1305, cipher_text: &str) -> Result<String> {
    let bytes = crypto::from_hex(cipher_text).ok_or_else(broken)?;
    if bytes.len() <= NONCE_BYTES {
        return Err(broken());
    }
    let (nonce_bytes, body) = bytes.split_at(NONCE_BYTES);

    let plain = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), body)
        .map_err(|_| broken())?;
    String::from_utf8(plain).map_err(|_| broken())
}

fn broken() -> Error {
    Error::msg("暗号文を元に戻せませんでした")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストは固定の鍵で試す。`APP_KEY` の設定順に左右されないようにするため。
    fn test_cipher() -> ChaCha20Poly1305 {
        cipher_from(b"0123456789abcdef0123456789abcdef")
    }

    #[test]
    fn 往復できる() {
        let cipher = test_cipher();
        let encrypted = encrypt_with(&cipher, "ひみつの文字列").unwrap();
        assert_eq!(decrypt_with(&cipher, &encrypted).unwrap(), "ひみつの文字列");
    }

    #[test]
    fn 毎回違う暗号文になる() {
        let cipher = test_cipher();
        let a = encrypt_with(&cipher, "same").unwrap();
        let b = encrypt_with(&cipher, "same").unwrap();
        assert_ne!(a, b, "nonce が毎回変わる");
        assert_eq!(decrypt_with(&cipher, &a).unwrap(), "same");
        assert_eq!(decrypt_with(&cipher, &b).unwrap(), "same");
    }

    #[test]
    fn 改ざんは検出できる() {
        let cipher = test_cipher();
        let encrypted = encrypt_with(&cipher, "ひみつ").unwrap();
        // 最後の1文字を変える。
        let mut broken = encrypted[..encrypted.len() - 1].to_string();
        broken.push(if encrypted.ends_with('0') { '1' } else { '0' });
        assert!(decrypt_with(&cipher, &broken).is_err());
    }

    #[test]
    fn 鍵が違えば戻せない() {
        let encrypted = encrypt_with(&test_cipher(), "ひみつ").unwrap();
        let other = cipher_from(b"ffffffffffffffffffffffffffffffff");
        assert!(decrypt_with(&other, &encrypted).is_err());
    }

    #[test]
    fn 壊れた入力は同じエラーになる() {
        let cipher = test_cipher();
        for broken in ["", "zz", "00", "0011223344556677889900aa"] {
            let error = decrypt_with(&cipher, broken).unwrap_err();
            assert_eq!(
                error.to_string(),
                "暗号文を元に戻せませんでした",
                "{broken}"
            );
        }
    }

    #[test]
    fn 空の文字列も扱える() {
        let cipher = test_cipher();
        let encrypted = encrypt_with(&cipher, "").unwrap();
        assert_eq!(decrypt_with(&cipher, &encrypted).unwrap(), "");
    }

    #[test]
    fn app_keyが空ならエラーになる() {
        // `cipher()` は AppConfig の鍵を使う。空のときの案内を確かめる。
        if crate::config_registry::app_config().key.is_empty() {
            // ChaCha20Poly1305 は Debug を実装していないので unwrap_err は使えない。
            let error = match cipher() {
                Ok(_) => panic!("鍵が空なのに作れてしまった"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("key:generate"), "{error}");
        }
    }
}
