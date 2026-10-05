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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::http::{BoxFuture, Middleware, Next, Request};

/// 数え方の置き場所。
///
/// **プロセスのメモリに置きます。** プロセスを2つ動かすと、それぞれが別々に数えます
/// （実際の上限は指定の2倍になります）。厳密に守りたいときは、
/// 前段のプロキシか、共通の置き場所（Redis など）が要ります。
/// いまは外部の置き場所を持っていないので、この形にしてあります。
pub struct Throttle {
    max: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, Bucket>>,
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
            buckets: Mutex::new(HashMap::new()),
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
        Ok(Self::new(max, Duration::from_secs(minutes * 60)))
    }

    /// 1回数える。通ってよければ残り回数、駄目なら待ち時間（秒）を返す。
    fn hit(&self, key: &str, now: Instant) -> Result<u32, u64> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());

        // ついでに期限切れを捨てる。放っておくと際限なく増える。
        if buckets.len() > 10_000 {
            buckets.retain(|_, b| b.resets_at > now);
        }

        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            count: 0,
            resets_at: now + self.window,
        });
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
}

/// 誰を数えるかを決める。
///
/// いまはログインの仕組みが無いので、`X-Forwarded-For` の先頭か接続元のアドレスを使います。
/// どちらも無ければ、パスごとにまとめて数えます。
fn key_for(req: &Request) -> String {
    let who = req
        .header("x-forwarded-for")
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| req.header("x-real-ip"))
        .unwrap_or("unknown");
    format!("{who}|{}", req.path())
}

impl Middleware for Throttle {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let key = key_for(&req);
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

    #[test]
    fn 相手の見分け方() {
        let req = Request::new("GET", "/login");
        assert_eq!(key_for(&req), "unknown|/login");

        let req = Request::new("GET", "/login")
            .with_headers(vec![("x-forwarded-for".into(), "1.2.3.4, 5.6.7.8".into())]);
        assert_eq!(key_for(&req), "1.2.3.4|/login", "先頭だけを使う");

        let req = Request::new("GET", "/login")
            .with_headers(vec![("x-real-ip".into(), "9.9.9.9".into())]);
        assert_eq!(key_for(&req), "9.9.9.9|/login");
    }

    #[test]
    fn 置き場所が増えすぎたら掃除する() {
        let throttle = Throttle::new(1, Duration::from_secs(1));
        let now = Instant::now();
        for i in 0..10_001 {
            let _ = throttle.hit(&format!("key-{i}"), now);
        }
        let later = now + Duration::from_secs(2);
        let _ = throttle.hit("trigger", later);
        let count = throttle.buckets.lock().unwrap().len();
        assert!(count < 10_001, "期限切れを捨てている（残り {count}）");
    }
}
