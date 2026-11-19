//! A shared byte-rate limiter. Every connection of every provider asks it for permission before
//! sending a body, so the configured rate is the process-wide aggregate (the Go `SharedThrottle`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Paces callers to `bytes_per_sec` on average. A rate of 0 means unlimited.
pub struct Throttle {
    bytes_per_sec: u64,
    /// The instant at which the budget granted so far is used up.
    next_free: Mutex<Instant>,
}

impl Throttle {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec,
            next_free: Mutex::new(Instant::now()),
        }
    }

    pub fn is_unlimited(&self) -> bool {
        self.bytes_per_sec == 0
    }

    /// Reserves `bytes` and returns how long the caller must wait before sending them.
    pub fn reserve(&self, bytes: u64) -> Duration {
        if self.bytes_per_sec == 0 {
            return Duration::ZERO;
        }
        let cost = Duration::from_secs_f64(bytes as f64 / self.bytes_per_sec as f64);
        let mut next = self.next_free.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let start = (*next).max(now);
        *next = start + cost;
        start.saturating_duration_since(now)
    }

    /// Waits until `bytes` may be sent.
    pub async fn acquire(&self, bytes: u64) {
        let wait = self.reserve(bytes);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_waits() {
        let t = Throttle::new(0);
        assert!(t.is_unlimited());
        assert_eq!(t.reserve(1 << 30), Duration::ZERO);
    }

    #[test]
    fn reservations_queue_up_at_the_configured_rate() {
        let t = Throttle::new(1000); // 1000 B/s
        assert_eq!(t.reserve(500), Duration::ZERO, "first send is immediate");
        let w2 = t.reserve(500);
        assert!(
            w2 >= Duration::from_millis(450) && w2 <= Duration::from_millis(500),
            "{w2:?}"
        );
        let w3 = t.reserve(1000);
        assert!(
            w3 >= Duration::from_millis(950) && w3 <= Duration::from_millis(1000),
            "{w3:?}"
        );
    }

    #[tokio::test]
    async fn acquire_paces_aggregate_throughput() {
        let t = std::sync::Arc::new(Throttle::new(100_000));
        let start = Instant::now();
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let t = t.clone();
                tokio::spawn(async move { t.acquire(10_000).await })
            })
            .collect();
        for h in hs {
            h.await.unwrap();
        }
        // 4 x 10 kB at 100 kB/s: the last one waits ~0.3 s
        assert!(
            start.elapsed() >= Duration::from_millis(250),
            "{:?}",
            start.elapsed()
        );
    }
}
