//! パーセントエンコードとデコード。
//!
//! 場所ごとに違うのは**そのまま残す文字の集合だけ**です。規則を1か所に置いて、
//! 署名の組み立て（`url.rs`）・ルートの組み立て（`routing.rs`）・Cookie（`cookie.rs`）が
//! 食い違わないようにしてあります。片方だけ直すと署名が合わなくなるためです。

/// 16進の大文字。`%XX` の形で書きます。
const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// `keep` が偽を返したバイトを `%XX` にする。
pub(crate) fn encode(input: &str, keep: fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let byte = *byte;
        if keep(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

/// URL にそのまま書ける文字（RFC 3986 の unreserved）。
pub(crate) fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

/// `unreserved` に `/` を足したもの。
///
/// `{*path}` のように、複数のセグメントを受けるパス引数で使います。
pub(crate) fn unreserved_or_slash(byte: u8) -> bool {
    unreserved(byte) || byte == b'/'
}

/// Cookie の名前と値にそのまま書ける文字。
///
/// 区切りに使う文字（`;` `,` 空白 `=`）と、制御文字・非 ASCII は逃がします。
pub(crate) fn cookie_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'_'
                | b'.'
                | b'~'
                | b'!'
                | b'#'
                | b'$'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'/'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b']'
                | b'^'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
        )
}

/// `%xx` と `+` を元に戻す。壊れた並びはそのまま残します。
pub(crate) fn decode(input: &str) -> String {
    decode_with(input, true)
}

/// `%xx` だけを元に戻す。`+` は空白にしません。
///
/// `+` が空白なのはフォームの書き方で、URL のパスや Cookie の値には当てはまりません。
pub(crate) fn decode_strict(input: &str) -> String {
    decode_with(input, false)
}

fn decode_with(input: &str, plus_is_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 残す文字はそのまま出す() {
        assert_eq!(encode("a-b_c.d~e", unreserved), "a-b_c.d~e");
        assert_eq!(encode("abcXYZ019", unreserved), "abcXYZ019");
    }

    #[test]
    fn 逃がす文字は大文字の16進にする() {
        assert_eq!(encode("a b?c#d", unreserved), "a%20b%3Fc%23d");
        assert_eq!(encode("あ", unreserved), "%E3%81%82");
        assert_eq!(encode("a/b", unreserved), "a%2Fb");
    }

    #[test]
    fn スラッシュを残す形もある() {
        assert_eq!(encode("a/b.txt", unreserved_or_slash), "a/b.txt");
        assert_eq!(encode("a b/c", unreserved_or_slash), "a%20b/c");
    }

    #[test]
    fn cookieは区切りの文字だけ逃がす() {
        assert_eq!(encode("x; y=z", cookie_safe), "x%3B%20y%3Dz");
        assert_eq!(encode("a/b:c", cookie_safe), "a/b:c");
    }

    #[test]
    fn デコードは往復する() {
        for keep in [unreserved as fn(u8) -> bool, cookie_safe] {
            for text in ["あ", "a b", "x; y=z", "a/b", ""] {
                assert_eq!(decode_strict(&encode(text, keep)), text, "{text}");
            }
        }
    }

    #[test]
    fn プラスの扱いが2通りある() {
        assert_eq!(decode("a+b"), "a b");
        assert_eq!(decode_strict("a+b"), "a+b");
    }

    #[test]
    fn 壊れた並びはそのまま残す() {
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%4"), "%4");
        assert_eq!(decode("%E3%81%82"), "あ");
    }
}
