//! 小数（`decimal` / `numeric`）の文字列をそろえる。
//! **MySQL と PostgreSQL で共通です。**
//!
//! どちらも小数は [`Value::Text`](super::Value::Text) で返します。浮動小数に
//! 直すと桁が落ちるためです。ところが末尾の 0 の数がドライバで違います。
//!
//! | 列と値                | MySQL が返す文字列 | PostgreSQL が返す文字列 |
//! |-----------------------|--------------------|-------------------------|
//! | `numeric(8,2)` に 12.5 | `12.50`            | `12.5000`               |
//!
//! PostgreSQL は数を4桁ずつの塊で送るので、桁数が4の倍数に膨らみます。
//! そのままだと、同じアプリが MySQL と PostgreSQL で違う文字列を受け取ります。
//! ここで末尾の 0 を落として、**どちらでも同じ文字列**にします。
//!
//! 桁を落とす向きの変換はしません。`12.345` は `12.345` のままです。

/// 末尾の 0 を落とす。小数点だけが残るときは、小数点も落とします。
///
/// 数として読めない文字列（`NaN` など）はそのまま返します。
pub(crate) fn normalize(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some((head, tail)) = trimmed.split_once('.') else {
        return trimmed.to_string();
    };
    // 小数部が数字だけでないときは触りません。`1.2e3` のような形を壊さないためです。
    if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return trimmed.to_string();
    }
    let kept = tail.trim_end_matches('0');
    if kept.is_empty() {
        head.to_string()
    } else {
        format!("{head}.{kept}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 末尾の0を落とす() {
        assert_eq!(normalize("12.50"), "12.5");
        assert_eq!(normalize("12.5000"), "12.5");
        assert_eq!(normalize("1234.5600"), "1234.56");
        assert_eq!(normalize("-0.500"), "-0.5");
    }

    #[test]
    fn 小数点だけ残るときは小数点も落とす() {
        assert_eq!(normalize("1.000"), "1");
        assert_eq!(normalize("0.00"), "0");
        assert_eq!(normalize("-0.0"), "-0");
    }

    #[test]
    fn 意味のある桁は残す() {
        assert_eq!(normalize("12.345"), "12.345");
        assert_eq!(normalize("100"), "100");
        assert_eq!(normalize("0"), "0");
        // 整数部の 0 は末尾ではないので残る。
        assert_eq!(normalize("1200"), "1200");
        assert_eq!(normalize("1200.00"), "1200");
    }

    #[test]
    fn 数として読めない文字列はそのまま返す() {
        assert_eq!(normalize("NaN"), "NaN");
        assert_eq!(normalize("Infinity"), "Infinity");
        // 指数の形を壊さない。
        assert_eq!(normalize("1.20e3"), "1.20e3");
        // 小数点で終わる形もそのまま。
        assert_eq!(normalize("12."), "12.");
    }

    #[test]
    fn 前後の空白は落とす() {
        assert_eq!(normalize("  12.50  "), "12.5");
    }
}
