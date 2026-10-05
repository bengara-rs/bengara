//! 署名に使う SHA-256 と HMAC、乱数、定数時間の比較。
//!
//! # なぜ自前で書くか
//!
//! SHA-256 と HMAC は仕様（FIPS 180-4 / RFC 2104）が公開されていて、公式のテストベクタで
//! 正しさを確かめられます。依存クレートを増やさない方針に合うので、この2つだけ自前で持ちます。
//!
//! **暗号化（中身を隠す処理）は自前で書きません。** ここにあるのは署名（改ざんの検出）だけです。
//! 乱数は OS から取ります（`getrandom`）。自分で作りません。

/// SHA-256 の出力（32 バイト）。
pub(crate) type Digest = [u8; 32];

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-256 を計算する。
pub(crate) fn sha256(input: &[u8]) -> Digest {
    let mut h = INIT;

    // 詰め物: 0x80 を1バイト、長さが 64 の倍数 - 8 になるまで 0、最後に元の長さ（ビット数）。
    let mut message = Vec::with_capacity(input.len() + 72);
    message.extend_from_slice(input);
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// HMAC-SHA256（RFC 2104）。鍵つきの署名を作ります。
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Digest {
    const BLOCK: usize = 64;

    // 鍵がブロックより長ければハッシュして縮める。短ければ 0 で埋める。
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(BLOCK + message.len());
    let mut outer = Vec::with_capacity(BLOCK + 32);
    for byte in k.iter() {
        inner.push(byte ^ 0x36);
        outer.push(byte ^ 0x5c);
    }
    inner.extend_from_slice(message);
    outer.extend_from_slice(&sha256(&inner));
    sha256(&outer)
}

/// PBKDF2-HMAC-SHA256（RFC 8018）。パスワードを総当たりしにくい形に変えます。
///
/// `iterations` 回 HMAC を繰り返します。回数が多いほど、1回の照合に時間がかかり、
/// 総当たりが割に合わなくなります。
///
/// 出力は 32 バイト（SHA-256 の1ブロックぶん）に固定しています。
/// それより長い鍵が要る使い方は、いまありません。
pub(crate) fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Digest {
    // ブロック番号は 1 つだけ（出力が 32 バイトのため）。
    let mut block = Vec::with_capacity(salt.len() + 4);
    block.extend_from_slice(salt);
    block.extend_from_slice(&1u32.to_be_bytes());

    let mut u = hmac_sha256(password, &block);
    let mut out = u;
    for _ in 1..iterations.max(1) {
        u = hmac_sha256(password, &u);
        for (slot, value) in out.iter_mut().zip(u.iter()) {
            *slot ^= value;
        }
    }
    out
}

/// バイト列を16進の小文字にする。
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 16進の文字列をバイト列に戻す。形が違えば `None`。
pub(crate) fn from_hex(input: &str) -> Option<Vec<u8>> {
    if input.len() % 2 != 0 {
        return None;
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(input.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// 長さと中身を**同じ時間で**比べる。
///
/// 普通の `==` は違いが見つかった時点で止まるため、かかった時間から
/// 「何文字目まで合っていたか」が漏れます。署名の照合にはこちらを使います。
///
/// 途中で `std::hint::black_box` を挟みます。この書き方で最適化に
/// 短絡された例はいまありませんが、コンパイラは「`diff` が 0 でなくなったら
/// 答えは決まる」と気づける形をしています。`black_box` は、その推論を
/// させないための目隠しです。依存クレートは増えません。
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
        diff = std::hint::black_box(diff);
    }
    std::hint::black_box(diff) == 0
}

/// OS から推測できない乱数をもらう。
///
/// # パニック
///
/// OS が乱数を返さないときはパニックします。推測できる値で代用すると、
/// セッションや CSRF の仕組みが成り立たなくなるためです。
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("OS から乱数を取得できませんでした");
    out
}

/// 推測できないトークンを作る（32 バイトを16進にした 64 文字）。
pub(crate) fn random_token() -> String {
    to_hex(&random_bytes::<32>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 の例と、広く知られたテストベクタ。
    #[test]
    fn sha256が公式のテストベクタと一致する() {
        assert_eq!(
            to_hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            to_hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            to_hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 100 万文字。詰め物とブロックの繰り返しをまとめて確かめる。
        assert_eq!(
            to_hex(&sha256(&[b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn sha256は詰め物の境目でも正しい() {
        // 55 / 56 / 63 / 64 バイトは詰め物の分岐が変わる長さ。
        let cases = [
            (
                55,
                "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
            ),
            (
                56,
                "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
            ),
            (
                63,
                "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
            ),
            (
                64,
                "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
            ),
        ];
        for (len, want) in cases {
            assert_eq!(to_hex(&sha256(&vec![b'a'; len])), want, "長さ {len}");
        }
    }

    /// RFC 4231 のテストケース 1・2・6。
    #[test]
    fn hmacが公式のテストベクタと一致する() {
        assert_eq!(
            to_hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            to_hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // 鍵がブロック（64 バイト）より長い場合は、先にハッシュして縮める。
        let long_key = [0xaa; 131];
        let long_msg: &[u8] = b"Test Using Larger Than Block-Size Key - Hash Key First";
        assert_eq!(
            to_hex(&hmac_sha256(&long_key, long_msg)),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn 鍵が違えば署名も変わる() {
        let a = hmac_sha256(b"key-a", b"same message");
        let b = hmac_sha256(b"key-b", b"same message");
        assert_ne!(a, b);
    }

    #[test]
    fn 十六進の変換が往復する() {
        let bytes = [0x00, 0x0f, 0xa5, 0xff];
        let hex = to_hex(&bytes);
        assert_eq!(hex, "000fa5ff");
        assert_eq!(from_hex(&hex).unwrap(), bytes);
        assert!(from_hex("abc").is_none(), "奇数の長さは断る");
        assert!(from_hex("zz").is_none(), "16 進でない文字は断る");
    }

    #[test]
    fn 定数時間の比較が正しく判定する() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn 定数時間の比較は最後まで見る() {
        // `black_box` を挟んでも判定が変わらないこと。
        // 先頭が違う・末尾が違う・1 バイトだけ違う、をそれぞれ確かめる。
        assert!(!constant_time_eq(b"Xbcdefgh", b"abcdefgh"), "先頭が違う");
        assert!(!constant_time_eq(b"abcdefgX", b"abcdefgh"), "末尾が違う");
        assert!(!constant_time_eq(&[0u8; 32], &[1u8; 32]));
        assert!(constant_time_eq(&[0u8; 32], &[0u8; 32]));
        // 署名の長さ（32 バイト）で、1 バイトだけ違う場合。
        let mut other = [0u8; 32];
        other[31] = 1;
        assert!(!constant_time_eq(&[0u8; 32], &other));
    }

    /// 広く使われている PBKDF2-HMAC-SHA256 のテストベクタ
    /// （RFC 8018 の手順に対する、SHA-256 版の既知の答え）。
    #[test]
    fn pbkdf2が公式のテストベクタと一致する() {
        assert_eq!(
            to_hex(&pbkdf2_sha256(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            to_hex(&pbkdf2_sha256(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
        assert_eq!(
            to_hex(&pbkdf2_sha256(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    #[test]
    fn pbkdf2は塩と回数で変わる() {
        let a = pbkdf2_sha256(b"same", b"salt-a", 10);
        let b = pbkdf2_sha256(b"same", b"salt-b", 10);
        let c = pbkdf2_sha256(b"same", b"salt-a", 11);
        assert_ne!(a, b, "塩が違えば結果も違う");
        assert_ne!(a, c, "回数が違えば結果も違う");
        // 0 回は 1 回として扱う（0 除算のような壊れ方をしないこと）。
        assert_eq!(pbkdf2_sha256(b"x", b"y", 0), pbkdf2_sha256(b"x", b"y", 1));
    }

    #[test]
    fn 乱数は毎回変わる() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// 回数を決めるための計測。`cargo test -- --ignored --nocapture` で見る。
    #[test]
    #[ignore]
    fn pbkdf2の速さを測る() {
        for iterations in [10_000u32, 50_000, 120_000, 600_000] {
            let started = Instant::now();
            let _ = pbkdf2_sha256(
                b"correct horse battery staple",
                b"0123456789abcdef",
                iterations,
            );
            println!("{iterations:>7} 回: {:?}", started.elapsed());
        }
    }
}
