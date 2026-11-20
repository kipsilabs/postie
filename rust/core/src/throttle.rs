//! Port of `internal/poster/throttle.go`: a token-bucket rate limiter (bytes per second). The bucket
//! starts with one second's worth of tokens and a consumer may overdraw it, then sleeps the deficit,
//! so the rate bounds aggregate egress. State is behind a mutex so concurrent callers cannot spend
//! the same tokens twice.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

struct Bucket {
    last: Instant,
    tokens: i64,
}

pub struct Throttle {
    rate: AtomicI64,
    state: Mutex<Bucket>,
}

impl Throttle {
    /// A rate of zero or less disables throttling.
    pub fn new(rate: i64) -> Self {
        Self::at(rate, Instant::now())
    }

    fn at(rate: i64, now: Instant) -> Self {
        Self {
            rate: AtomicI64::new(rate),
            state: Mutex::new(Bucket {
                last: now,
                tokens: rate.max(0),
            }),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.rate() <= 0
    }

    pub fn rate(&self) -> i64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Changes the rate in place (a settings change); holders of this throttle see it at once.
    pub fn set_rate(&self, rate: i64) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    /// How long a caller sending `bytes` at `now` must wait; spends the tokens.
    fn reserve_at(&self, now: Instant, bytes: i64) -> Duration {
        let rate = self.rate();
        if rate <= 0 {
            return Duration::ZERO;
        }
        let mut b = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let elapsed = now.saturating_duration_since(b.last).as_nanos() as i128;
        let to_add = (elapsed * i128::from(rate) / 1_000_000_000) as i64;
        b.last = now;
        let current = (b.tokens + to_add).min(rate);
        let wait_ns = if current < bytes {
            (i128::from(bytes - current) * 1_000_000_000 / i128::from(rate)) as u64
        } else {
            0
        };
        b.tokens = current - bytes;
        Duration::from_nanos(wait_ns)
    }

    /// The wait for sending `bytes` now (zero when disabled or within budget).
    pub fn reserve(&self, bytes: i64) -> Duration {
        self.reserve_at(Instant::now(), bytes)
    }

    /// Waits until `bytes` may be sent.
    pub async fn wait(&self, bytes: i64) {
        let d = self.reserve(bytes);
        if !d.is_zero() {
            tokio::time::sleep(d).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_for_zero_or_negative_rates() {
        assert!(Throttle::new(0).is_disabled() && Throttle::new(-100).is_disabled());
        assert!(!Throttle::new(1000).is_disabled());
        assert_eq!(Throttle::new(0).reserve(1_000_000), Duration::ZERO);
    }

    #[test]
    fn waits_follow_the_token_bucket_math() {
        let t0 = Instant::now();
        // under the rate: no sleep
        let t = Throttle::at(1000, t0);
        assert_eq!(t.reserve_at(t0, 100), Duration::ZERO);
        // 5x the rate: deficit 4000 B -> 4 s
        let t = Throttle::at(1000, t0);
        let w = t.reserve_at(t0, 5000);
        assert!(
            w > Duration::from_millis(3500) && w < Duration::from_millis(5500),
            "{w:?}"
        );
        // accumulated calls: 900 total is fine, the next 500 overdraws
        let t = Throttle::at(1000, t0);
        for _ in 0..3 {
            assert_eq!(t.reserve_at(t0, 300), Duration::ZERO);
        }
        assert!(t.reserve_at(t0, 500) > Duration::from_millis(300));
    }

    #[test]
    fn tokens_refill_over_time_up_to_one_second() {
        let t0 = Instant::now();
        let t = Throttle::at(1000, t0);
        assert_eq!(t.reserve_at(t0, 1000), Duration::ZERO);
        // half a second later 500 tokens are back
        assert_eq!(
            t.reserve_at(t0 + Duration::from_millis(500), 500),
            Duration::ZERO
        );
        // an hour later the bucket is capped at one second of tokens
        assert_eq!(
            t.reserve_at(t0 + Duration::from_secs(3600), 1000),
            Duration::ZERO
        );
        assert!(t.reserve_at(t0 + Duration::from_secs(3600), 1000) >= Duration::from_millis(900));
    }

    #[tokio::test]
    async fn disabled_wait_is_immediate() {
        let start = Instant::now();
        let t = Throttle::new(0);
        for _ in 0..10 {
            t.wait(10_000).await;
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
