//! アクセス回数の制限。Laravel の `throttle` に当たります。
//!
//! ```ignore
//! // bootstrap/app.rs
//! .alias("throttle", Throttle::per_minute(60))
//!
//! // routes/web.rs
//! Route::post("/login", AuthController::login).middleware("throttle");
//! ```
//!
//! 超えると 429 を返し、`Retry-After` と `X-RateLimit-*` を付けます。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::http::{BoxFuture, Middleware, Next, Request};

/// 置き場所に置ける鍵の上限。
///
/// 超えたときは `count` の多い鍵（制限に近いもの）を残して落とします。詳しくは `sweep`。
const MAX_BUCKETS: usize = 10_000;

/// これを超えたら、掃除の間隔を待たずにその場で落とす硬い上限。
///
/// `sweep` は `window`（既定 60 秒）に1回しか走りません。その間に鍵を増やされても
/// メモリを食いつぶされないように、挿入のときだけこの線を見ます。
const HARD_MAX_BUCKETS: usize = MAX_BUCKETS * 2;

/// 信頼する前段のプロキシを決める環境変数の名前。
///
/// カンマ区切りの IP アドレスの一覧です。特別な値 `*` は「すべて信頼する」。
/// 未設定（空）なら、`X-Forwarded-For` と `X-Real-IP` を**どちらも見ません**。
///
/// `10.0.0.0/8` のような範囲（CIDR）の書き方は読めません。依存クレートを増やさないため、
/// 範囲の解析は実装していません。アドレスを1つずつ並べてください。
const TRUSTED_PROXIES: &str = "TRUSTED_PROXIES";

/// 数え方の置き場所。
///
/// **プロセスのメモリに置きます。** プロセスを2つ動かすと、それぞれが別々に数えます
/// （実際の上限は指定の2倍になります）。厳密に守りたいときは、
/// 前段のプロキシか、共通の置き場所（Redis など）が要ります。
/// いまは外部の置き場所を持っていないので、この形にしてあります。
pub struct Throttle {
    max: u32,
    window: Duration,
    buckets: Mutex<Buckets>,
    /// 鍵を作れなかったときの警告を、1回だけ出すための目印。
    warned: AtomicBool,
    /// 件数の天井で鍵を落としたことを、1回だけ知らせるための目印。
    evicted: AtomicBool,
}

/// 数え方の中身と、最後に掃除した時刻。
struct Buckets {
    map: HashMap<String, Bucket>,
    last_swept: Instant,
}

#[derive(Clone, Copy)]
struct Bucket {
    count: u32,
    /// この時刻を過ぎたら数え直す。
    resets_at: Instant,
}

impl Throttle {
    /// 回数と期間を決めて作る。
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            buckets: Mutex::new(Buckets {
                map: HashMap::new(),
                last_swept: Instant::now(),
            }),
            warned: AtomicBool::new(false),
            evicted: AtomicBool::new(false),
        }
    }

    /// 1分あたりの回数で作る。
    pub fn per_minute(max: u32) -> Self {
        Self::new(max, Duration::from_secs(60))
    }

    /// `60,1`（1分あたり60回）の形から作る。Laravel の `throttle:60,1` と同じ書き方です。
    pub fn from_spec(spec: &str) -> crate::error::Result<Self> {
        let (max, minutes) = match spec.split_once(',') {
            Some((m, w)) => (m.trim(), w.trim()),
            None => (spec.trim(), "1"),
        };
        let max: u32 = max
            .parse()
            .map_err(|_| Error::msg(format!("throttle の回数 `{max}` が数値ではありません")))?;
        let minutes: u64 = minutes
            .parse()
            .map_err(|_| Error::msg(format!("throttle の分 `{minutes}` が数値ではありません")))?;
        if max == 0 || minutes == 0 {
            return Err(Error::msg("throttle の回数と分は 1 以上にしてください"));
        }
        let seconds = minutes
            .checked_mul(60)
            .ok_or_else(|| Error::msg(format!("throttle の分 `{minutes}` が大きすぎます")))?;
        Ok(Self::new(max, Duration::from_secs(seconds)))
    }

    /// 1回数える。通ってよければ残り回数、駄目なら待ち時間（秒）を返す。
    fn hit(&self, key: &str, now: Instant) -> Result<u32, u64> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        self.sweep(&mut buckets, now);

        // 既にある鍵のときは `String` を作らない（`entry` は毎回 `to_string` が要る）。
        if !buckets.map.contains_key(key) {
            // 掃除の間隔を待つと、その間は無制限に鍵が増えます。
            // 硬い上限だけは、挿入の直前にその場で守ります。
            if buckets.map.len() > HARD_MAX_BUCKETS {
                self.evict(&mut buckets, now);
            }
            buckets.map.insert(
                key.to_string(),
                Bucket {
                    count: 0,
                    resets_at: now + self.window,
                },
            );
        }
        let bucket = buckets.map.get_mut(key).expect("直前に無ければ入れている");
        if bucket.resets_at <= now {
            bucket.count = 0;
            bucket.resets_at = now + self.window;
        }
        if bucket.count >= self.max {
            let wait = bucket.resets_at.saturating_duration_since(now).as_secs() + 1;
            return Err(wait);
        }
        bucket.count += 1;
        Ok(self.max - bucket.count)
    }

    /// 期限切れを捨てる。`window` に1回だけ走ります。
    ///
    /// 件数で決めると、期限内の鍵が天井を超えたままになったときに、1件も落とせない
    /// `retain` をロックの中で毎リクエスト走らせてしまいます。時刻で間隔を決めれば、
    /// どれだけ混んでも `window` に1回で済みます。
    fn sweep(&self, buckets: &mut Buckets, now: Instant) {
        if now.saturating_duration_since(buckets.last_swept) < self.window {
            return;
        }
        buckets.last_swept = now;
        self.evict(buckets, now);
    }

    /// 期限切れを捨て、それでも天井を超えるなら件数で落とす。
    ///
    /// `sweep`（間隔つき）と、硬い上限を超えたとき（`hit` の挿入の直前）で共用します。
    fn evict(&self, buckets: &mut Buckets, now: Instant) {
        buckets.map.retain(|_, b| b.resets_at > now);
        if buckets.map.len() <= MAX_BUCKETS {
            return;
        }
        // 掃除しても天井を超えるなら、件数を抑えるために鍵を落とす。
        //
        // **残すのは制限に近い鍵（`count` の多いもの）です。** 以前は期限の新しい順に
        // 残していたので、鍵を大量に作った相手の分が守られ、先に上限へ達していた
        // `/login` の鍵が落ちて数え直しになっていました（攻撃者に有利）。
        //
        // 全体は並べ替えません。以前は最大2万件を鍵ごと clone して
        // `sort_unstable_by` に掛け、その間ずっと `buckets` の錠を握っていました
        // （回数制限のかかる全リクエストがこの錠を取ります）。
        // いま採るのは「`MAX_BUCKETS` 番目だけを確定させて、その境目で振り分ける」形です。
        // 鍵は clone せず、並べ替えの材料（`count` と `resets_at`）だけを集めます。
        // **どれを残すかの方針は同じ**です（`count` の多い順、同数なら `resets_at` の遠い順）。
        let order = |a: &(u32, Instant), b: &(u32, Instant)| b.0.cmp(&a.0).then(b.1.cmp(&a.1));
        let mut ranks: Vec<(u32, Instant)> = buckets
            .map
            .values()
            .map(|bucket| (bucket.count, bucket.resets_at))
            .collect();
        let border = *ranks.select_nth_unstable_by(MAX_BUCKETS, order).1;
        // 境目と同じ値の鍵は、残せる数だけ残す（同じ値が境目にまたがるため）。
        let stronger = ranks[..MAX_BUCKETS]
            .iter()
            .filter(|rank| order(rank, &border) == std::cmp::Ordering::Less)
            .count();
        let mut allowance = MAX_BUCKETS - stronger;
        let before = buckets.map.len();
        buckets.map.retain(|_, bucket| {
            match order(&(bucket.count, bucket.resets_at), &border) {
                // 境目より強い（制限に近い）。必ず残す。
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Equal if allowance > 0 => {
                    allowance -= 1;
                    true
                }
                _ => false,
            }
        });
        let dropped = before - buckets.map.len();
        if dropped > 0 && !self.evicted.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "数え方の置き場所が {MAX_BUCKETS} 件を超えたので、{dropped} 件を落としました。\
                 回数を数える鍵が増えすぎています"
            );
        }
    }

    /// 鍵を作れなかったことを1回だけ知らせる。
    fn warn_no_key(&self) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "接続元のアドレスが分からないため、throttle を掛けずに通します。\
                 アドレスを入れずに Request を手で組み立てたときに起きます\
                 （#[bengara::test] からの呼び出しは 127.0.0.1 になるので掛かります）"
            );
        }
    }
}

/// 信頼する前段のプロキシ。
#[derive(Debug, PartialEq, Eq)]
enum Trusted {
    /// 1つも信頼しない（既定）。ヘッダーは見ません。
    None,
    /// すべて信頼する（`*`）。
    All,
    /// この一覧だけ信頼する。
    List(Vec<IpAddr>),
}

impl Trusted {
    /// この相手の言うことを聞いてよいか。
    fn allows(&self, peer: &IpAddr) -> bool {
        match self {
            Trusted::None => false,
            Trusted::All => true,
            Trusted::List(list) => list.contains(peer),
        }
    }
}

/// `TRUSTED_PROXIES` を読む。起動後は読むだけなので、1回だけ解析します。
fn trusted_proxies() -> &'static Trusted {
    static CACHE: OnceLock<Trusted> = OnceLock::new();
    CACHE.get_or_init(|| parse_trusted_proxies(&std::env::var(TRUSTED_PROXIES).unwrap_or_default()))
}

/// `TRUSTED_PROXIES` の値を解析する。
fn parse_trusted_proxies(raw: &str) -> Trusted {
    let mut list = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "*" {
            return Trusted::All;
        }
        match part.parse::<IpAddr>() {
            Ok(ip) => list.push(ip),
            Err(_) => tracing::warn!(
                "{TRUSTED_PROXIES} の `{part}` は IP アドレスとして読めないため無視します\
                 （範囲の書き方には対応していません）"
            ),
        }
    }
    if list.is_empty() {
        Trusted::None
    } else {
        Trusted::List(list)
    }
}

/// 誰を数えるかを決める。鍵を作れないときは `None`。
///
/// ログインしていれば**その人ごと**に数えます。していなければ**接続元のアドレス**ごとです。
/// ログイン中の人ごとに数えると、同じ回線にいる別の人（会社や学校のネットワーク）が
/// 巻き込まれません。
///
/// `X-Forwarded-For` と `X-Real-IP` は、接続元が `TRUSTED_PROXIES` に書かれた相手の
/// ときだけ見ます。無条件に信じると、偽の値を送るだけで回数制限を無制限に回せます。
fn key_for(req: &Request) -> Option<String> {
    key_for_with(req, trusted_proxies())
}

fn key_for_with(req: &Request, trusted: &Trusted) -> Option<String> {
    let target = target_of(req);
    if let Some(id) = req.auth().id() {
        return Some(format!("user:{id}|{target}"));
    }
    // 接続元が分からなければ鍵を作らない。`"unknown"` のような1つの鍵にまとめると、
    // 誰か1人が使い切るだけで全員を締め出せてしまいます。
    let peer = req.ip()?;
    // ヘッダーから採るのも `IpAddr` です。`user:5` のような文字列は鍵になりません。
    let who = if trusted.allows(&peer) {
        forwarded_ip(req, trusted).unwrap_or(peer)
    } else {
        peer
    };
    Some(format!("{who}|{target}"))
}

/// 何に対する回数かを表す文字列。
///
/// **ルート名があればそちらを使います。** 生のパスを使うと、`/api/items/{id}` の
/// ようなパス引数つきのルートで、1つの相手が `/api/items/1`…`/api/items/20000` と
/// 叩くだけで鍵を好きなだけ増やせます。ルート名は起動時に一意だと確かめてあるので、
/// パス引数の数だけ鍵が増えることがありません。
///
/// 名前を付けていないルートと、ルートに当たらなかったときはパスを使います。
fn target_of(req: &Request) -> &str {
    req.route_name().unwrap_or_else(|| req.path())
}

/// 前段のプロキシが伝えてきた、元の相手。
///
/// `X-Forwarded-For` は「クライアント, プロキシ1, プロキシ2」の順に並びます。
/// nginx の `$proxy_add_x_forwarded_for` は**クライアントが送った値に追記する**ので、
/// 先頭は相手が自由に決められます。検査せずに鍵にすると、ヘッダーを毎回変えるだけで
/// 回数制限を回せます。`user:5` のような値を送れば、利用者 ID 5 の枠も使い切れます。
///
/// そこで、**必ず `IpAddr` として読める要素だけを使います。** 読めない要素は捨てます。
/// どれを採るかは信頼の設定で変えます。
///
/// | 設定 | 採る場所 |
/// |---|---|
/// | `Trusted::List` | **右端から**見て、信頼一覧に無い最初のアドレス |
/// | `Trusted::All` | 先頭（右端から辿る手がかりが無い） |
///
/// `Trusted::List` で右端から辿るのは Laravel / Symfony と同じ考え方です。
/// 自分の前段を右から順に取り除くと、残った右端が本当の相手になります。
///
/// 1つも採れなければ `X-Real-IP` を見て、それも読めなければ `None`
/// （呼ぶ側が接続元のアドレスに落とします）。
fn forwarded_ip(req: &Request, trusted: &Trusted) -> Option<IpAddr> {
    let parse = |part: &str| part.trim().parse::<IpAddr>().ok();
    if let Some(raw) = req.header("x-forwarded-for") {
        let found = match trusted {
            Trusted::None => None,
            Trusted::All => raw.split(',').filter_map(parse).next(),
            Trusted::List(_) => raw
                .split(',')
                .rev()
                .filter_map(parse)
                .find(|ip| !trusted.allows(ip)),
        };
        if found.is_some() {
            return found;
        }
    }
    req.header("x-real-ip").and_then(parse)
}

impl Middleware for Throttle {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        // 鍵を作れないときは、制限を掛けずに通します。全員で1つの鍵を共有すると、
        // 1人が使い切るだけで他の全員を締め出せる（止めたい攻撃そのものになる）ためです。
        // 実際のサーバーでは接続元が必ず分かるので、ここに来るのはテストのときだけです。
        let Some(key) = key_for(&req) else {
            self.warn_no_key();
            return next.run(req);
        };
        let result = self.hit(&key, Instant::now());
        let max = self.max;
        let options = next.render_options();

        Box::pin(async move {
            match result {
                Ok(remaining) => {
                    let response = next.run(req).await?;
                    Ok(response
                        .with_header("x-ratelimit-limit", max.to_string())
                        .with_header("x-ratelimit-remaining", remaining.to_string()))
                }
                Err(wait) => {
                    // Retry-After を付けたいので、エラーではなくレスポンスを自分で組み立てる。
                    let error = Error::http(
                        429,
                        format!("アクセスが多すぎます。{wait} 秒ほど待ってからお試しください。"),
                    );
                    tracing::debug!("{error}");
                    Ok(crate::http::response::error_response(&error, options)
                        .with_header("retry-after", wait.to_string())
                        .with_header("x-ratelimit-limit", max.to_string())
                        .with_header("x-ratelimit-remaining", "0"))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 上限まで通り超えると断る() {
        let throttle = Throttle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        assert_eq!(throttle.hit("a", now), Ok(2));
        assert_eq!(throttle.hit("a", now), Ok(1));
        assert_eq!(throttle.hit("a", now), Ok(0));
        assert!(throttle.hit("a", now).is_err(), "4回目は断る");
    }

    #[test]
    fn 相手が違えば別に数える() {
        let throttle = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(throttle.hit("a", now).is_ok());
        assert!(throttle.hit("a", now).is_err());
        assert!(throttle.hit("b", now).is_ok(), "別の相手は巻き込まない");
    }

    #[test]
    fn 期間を過ぎたら数え直す() {
        let throttle = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(throttle.hit("a", now).is_ok());
        assert!(throttle.hit("a", now).is_err());

        let later = now + Duration::from_secs(61);
        assert!(throttle.hit("a", later).is_ok(), "期間を過ぎたら通る");
    }

    #[test]
    fn 待ち時間を返す() {
        let throttle = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        let _ = throttle.hit("a", now);
        let wait = throttle.hit("a", now).unwrap_err();
        assert!((1..=61).contains(&wait), "待ち時間は {wait} 秒");
    }

    #[test]
    fn 書き方から作れる() {
        assert!(Throttle::from_spec("60,1").is_ok());
        assert!(Throttle::from_spec("60").is_ok(), "分を省くと 1 分");
        assert!(Throttle::from_spec(" 10 , 5 ").is_ok());
        assert!(Throttle::from_spec("abc").is_err());
        assert!(Throttle::from_spec("0,1").is_err(), "0 回は意味がない");
        assert!(Throttle::from_spec("1,0").is_err());
    }

    /// 接続元のアドレスを載せたリクエストを作る。
    fn from(peer: &str) -> Request {
        let addr: std::net::SocketAddr = format!("{peer}:12345").parse().expect("アドレスの形");
        Request::new("GET", "/login").with_remote_addr(addr)
    }

    #[test]
    fn 接続元が分からなければ鍵を作らない() {
        let req = Request::new("GET", "/login");
        assert_eq!(key_for_with(&req, &Trusted::All), None);
    }

    #[test]
    fn 既定では接続元のアドレスで数える() {
        let req = from("203.0.113.9");
        assert_eq!(
            key_for_with(&req, &Trusted::None),
            Some("203.0.113.9|/login".to_string())
        );
    }

    #[test]
    fn 名前が付いていればルート名で数える() {
        // パス引数つきのルートで、パスごとに鍵が増えないようにする。
        let addr: std::net::SocketAddr = "203.0.113.9:12345".parse().expect("アドレスの形");
        let mut first = Request::new("GET", "/api/items/1").with_remote_addr(addr);
        first.set_route_name(Some("items.show".into()));
        let mut second = Request::new("GET", "/api/items/99").with_remote_addr(addr);
        second.set_route_name(Some("items.show".into()));

        assert_eq!(
            key_for_with(&first, &Trusted::None),
            key_for_with(&second, &Trusted::None),
            "同じルートなら、パスが違っても同じ鍵になる"
        );
        assert_eq!(
            key_for_with(&first, &Trusted::None),
            Some("203.0.113.9|items.show".to_string())
        );
    }

    #[test]
    fn 制限に近い鍵を残して落とす() {
        let throttle = Throttle::new(10, Duration::from_secs(60));
        let base = Instant::now() + Duration::from_secs(60);
        // まず1回掃除を走らせ、基準の時刻をそろえる。
        let _ = throttle.hit("first", base);

        let filling = base + Duration::from_secs(30);
        // 1件だけ上限に近づけ、残りは1回ずつにする。
        for _ in 0..9 {
            let _ = throttle.hit("close-to-limit", filling);
        }
        for i in 0..(MAX_BUCKETS + 100) {
            let _ = throttle.hit(&format!("key-{i}"), filling);
        }

        let sweeping = base + Duration::from_secs(60);
        let _ = throttle.hit("trigger", sweeping);

        let buckets = throttle.buckets.lock().unwrap();
        assert!(buckets.map.len() <= MAX_BUCKETS + 1, "天井を守っている");
        assert!(
            buckets.map.contains_key("close-to-limit"),
            "制限に近い鍵は残す（落とすと数え直しになる）"
        );
    }

    #[test]
    fn 信頼していない相手のヘッダーは見ない() {
        let req = from("203.0.113.9").with_headers(vec![
            ("x-forwarded-for".into(), "1.2.3.4, 5.6.7.8".into()),
            ("x-real-ip".into(), "9.9.9.9".into()),
        ]);
        // 偽の値を送られても、接続元のアドレスで数える。
        assert_eq!(
            key_for_with(&req, &Trusted::None),
            Some("203.0.113.9|/login".to_string())
        );
        let others = Trusted::List(vec!["10.0.0.1".parse().unwrap()]);
        assert_eq!(
            key_for_with(&req, &others),
            Some("203.0.113.9|/login".to_string())
        );
    }

    #[test]
    fn 信頼している相手のヘッダーは使う() {
        let req = from("10.0.0.1")
            .with_headers(vec![("x-forwarded-for".into(), "1.2.3.4, 5.6.7.8".into())]);
        let trusted = Trusted::List(vec!["10.0.0.1".parse().unwrap()]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("5.6.7.8|/login".to_string()),
            "一覧で絞るときは右端から（1.2.3.4 は相手が足した値かもしれない）"
        );
        assert_eq!(
            key_for_with(&req, &Trusted::All),
            Some("1.2.3.4|/login".to_string()),
            "すべて信頼するときは右端から辿れないので先頭"
        );

        // X-Forwarded-For が無ければ X-Real-IP を見る。
        let req = from("10.0.0.1").with_headers(vec![("x-real-ip".into(), "9.9.9.9".into())]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("9.9.9.9|/login".to_string())
        );

        // どちらも無ければ接続元のアドレス。
        let req = from("10.0.0.1");
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("10.0.0.1|/login".to_string())
        );
    }

    #[test]
    fn 読めない値は鍵にしない() {
        let trusted = Trusted::List(vec!["10.0.0.1".parse().unwrap()]);

        // `user:5` を鍵にすると、その利用者の枠を外から使い切れてしまう。
        let req = from("10.0.0.1").with_headers(vec![("x-forwarded-for".into(), "user:5".into())]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("10.0.0.1|/login".to_string()),
            "読めない値は捨てて接続元のアドレスに落ちる"
        );
        assert_eq!(
            key_for_with(&req, &Trusted::All),
            Some("10.0.0.1|/login".to_string())
        );

        // 読めない要素が混じっていても、読める要素だけを使う。
        let req = from("10.0.0.1").with_headers(vec![(
            "x-forwarded-for".into(),
            "nonsense, 1.2.3.4, , bad".into(),
        )]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("1.2.3.4|/login".to_string())
        );
        assert_eq!(
            key_for_with(&req, &Trusted::All),
            Some("1.2.3.4|/login".to_string()),
            "先頭が読めなければ次を見る"
        );

        // X-Real-IP も検査を通す。
        let req = from("10.0.0.1").with_headers(vec![("x-real-ip".into(), "user:5".into())]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("10.0.0.1|/login".to_string())
        );

        // X-Forwarded-For が読めなければ X-Real-IP に落ちる。
        let req = from("10.0.0.1").with_headers(vec![
            ("x-forwarded-for".into(), "nonsense".into()),
            ("x-real-ip".into(), "9.9.9.9".into()),
        ]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("9.9.9.9|/login".to_string())
        );
    }

    #[test]
    fn 信頼一覧のときは右端から採る() {
        // nginx を2段重ねた形。右の2つは自分の前段なので、本当の相手は 203.0.113.9。
        let trusted = Trusted::List(vec![
            "10.0.0.1".parse().unwrap(),
            "10.0.0.2".parse().unwrap(),
        ]);
        let req = from("10.0.0.1").with_headers(vec![(
            "x-forwarded-for".into(),
            "1.2.3.4, 203.0.113.9, 10.0.0.2, 10.0.0.1".into(),
        )]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("203.0.113.9|/login".to_string()),
            "相手が足した 1.2.3.4 は使わない"
        );

        // 全部が信頼一覧なら、採れるものが無いので接続元のアドレスに落ちる。
        let req = from("10.0.0.1").with_headers(vec![(
            "x-forwarded-for".into(),
            "10.0.0.2, 10.0.0.1".into(),
        )]);
        assert_eq!(
            key_for_with(&req, &trusted),
            Some("10.0.0.1|/login".to_string())
        );
    }

    #[test]
    fn 硬い上限は掃除の間隔を待たずに守る() {
        let throttle = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        let swept = throttle.buckets.lock().unwrap().last_swept;
        // 掃除は `window` に1回しか走らないので、この間はずっと同じ時刻で呼ぶ。
        for i in 0..(HARD_MAX_BUCKETS + 10) {
            let _ = throttle.hit(&format!("key-{i}"), now);
        }
        let count = throttle.buckets.lock().unwrap().map.len();
        assert!(
            count <= MAX_BUCKETS + 10,
            "硬い上限を超えたらその場で落とす（残り {count}）"
        );
        assert_eq!(
            throttle.buckets.lock().unwrap().last_swept,
            swept,
            "掃除そのものは走っていない"
        );
    }

    #[test]
    fn 信頼するプロキシの一覧を解析できる() {
        assert_eq!(parse_trusted_proxies(""), Trusted::None);
        assert_eq!(parse_trusted_proxies("   "), Trusted::None);
        assert_eq!(parse_trusted_proxies(","), Trusted::None);
        assert_eq!(parse_trusted_proxies("*"), Trusted::All);
        assert_eq!(parse_trusted_proxies("10.0.0.1, *"), Trusted::All);
        assert_eq!(
            parse_trusted_proxies(" 10.0.0.1 , 10.0.0.2 "),
            Trusted::List(vec![
                "10.0.0.1".parse().unwrap(),
                "10.0.0.2".parse().unwrap(),
            ])
        );
        assert_eq!(
            parse_trusted_proxies("::1"),
            Trusted::List(vec!["::1".parse().unwrap()])
        );
        // 範囲（CIDR）は読めないので無視する。
        assert_eq!(parse_trusted_proxies("10.0.0.0/8"), Trusted::None);
        assert_eq!(
            parse_trusted_proxies("10.0.0.0/8, 10.0.0.1"),
            Trusted::List(vec!["10.0.0.1".parse().unwrap()])
        );
    }

    #[test]
    fn 期限切れを掃除する() {
        let throttle = Throttle::new(1, Duration::from_secs(1));
        let now = Instant::now();
        for i in 0..100 {
            let _ = throttle.hit(&format!("key-{i}"), now);
        }
        assert_eq!(throttle.buckets.lock().unwrap().map.len(), 100);

        let later = now + Duration::from_secs(2);
        let _ = throttle.hit("trigger", later);
        let count = throttle.buckets.lock().unwrap().map.len();
        assert_eq!(count, 1, "期限切れを捨てて、いま数えた1件だけが残る");
    }

    #[test]
    fn 短い間隔で呼んでも掃除は走らない() {
        let throttle = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        let _ = throttle.hit("a", now);
        let swept = throttle.buckets.lock().unwrap().last_swept;

        // window の中なので、何回呼んでも掃除は走らない。
        for i in 0..10 {
            let _ = throttle.hit(&format!("key-{i}"), now + Duration::from_secs(1));
        }
        assert_eq!(
            throttle.buckets.lock().unwrap().last_swept,
            swept,
            "掃除していない"
        );
        assert_eq!(throttle.buckets.lock().unwrap().map.len(), 11);
    }

    #[test]
    fn 掃除しても多すぎるなら天井を守る() {
        let throttle = Throttle::new(1, Duration::from_secs(60));
        let base = Instant::now() + Duration::from_secs(60);
        // まず1回掃除を走らせ、基準の時刻をそろえる。
        let _ = throttle.hit("first", base);

        // 期限が base + 90 の鍵を、天井より多く積む（まだ掃除は走らない）。
        let filling = base + Duration::from_secs(30);
        for i in 0..(MAX_BUCKETS + 500) {
            let _ = throttle.hit(&format!("key-{i}"), filling);
        }
        assert!(
            throttle.buckets.lock().unwrap().map.len() > MAX_BUCKETS,
            "掃除の前は超えていてよい"
        );

        // 掃除の時刻。積んだ鍵はまだ期限内なので、件数で落とすしかない。
        let sweeping = base + Duration::from_secs(60);
        let _ = throttle.hit("trigger", sweeping);
        let count = throttle.buckets.lock().unwrap().map.len();
        assert!(count <= MAX_BUCKETS + 1, "天井を守っている（残り {count}）");
    }

    #[test]
    fn 天井で落とすとき制限に近い鍵を残す() {
        // 全体を並べ替えない形にしたので、残す方針が変わっていないかを確かめる。
        // `count` の多い鍵（制限に近いもの）が残ること。以前ここを期限の新しい順に
        // していたので、鍵を大量に作った相手の分が守られていました（攻撃者に有利）。
        let throttle = Throttle::new(u32::MAX, Duration::from_secs(60));
        let now = Instant::now();

        {
            let mut buckets = throttle.buckets.lock().unwrap();
            // 制限に近い鍵を少しだけ。
            for i in 0..10 {
                buckets.map.insert(
                    format!("hot-{i}"),
                    Bucket {
                        count: 100,
                        resets_at: now + Duration::from_secs(10),
                    },
                );
            }
            // 1回しか叩いていない鍵を、天井を超えるまで。
            for i in 0..(MAX_BUCKETS + 500) {
                buckets.map.insert(
                    format!("cold-{i}"),
                    Bucket {
                        count: 1,
                        // 期限は `hot` より遠い。期限で残すと `hot` が落ちる。
                        resets_at: now + Duration::from_secs(50),
                    },
                );
            }
        }

        let mut buckets = throttle.buckets.lock().unwrap();
        throttle.evict(&mut buckets, now);
        assert_eq!(buckets.map.len(), MAX_BUCKETS, "天井ぴったりまで落とす");
        for i in 0..10 {
            assert!(
                buckets.map.contains_key(&format!("hot-{i}")),
                "制限に近い hot-{i} は残る"
            );
        }
    }

    #[test]
    fn 同じcountなら期限の遠いほうを残す() {
        let throttle = Throttle::new(u32::MAX, Duration::from_secs(60));
        let now = Instant::now();

        {
            let mut buckets = throttle.buckets.lock().unwrap();
            // どれも `count` は同じ。期限だけが違う。
            for i in 0..(MAX_BUCKETS + 100) {
                buckets.map.insert(
                    format!("key-{i}"),
                    Bucket {
                        count: 5,
                        // i が大きいほど期限が遠い。
                        resets_at: now + Duration::from_secs(10 + i as u64),
                    },
                );
            }
        }

        let mut buckets = throttle.buckets.lock().unwrap();
        throttle.evict(&mut buckets, now);
        assert_eq!(buckets.map.len(), MAX_BUCKETS);
        // 期限が最も近い（i の小さい）ものが落ちる。
        assert!(!buckets.map.contains_key("key-0"));
        assert!(buckets
            .map
            .contains_key(&format!("key-{}", MAX_BUCKETS + 99)));
    }
}
