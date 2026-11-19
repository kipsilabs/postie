//! Port of `internal/poster/uploadmeter.go`: counts article payload bytes handed to the NNTP pool.
//! The pool only reports a few hundred bytes of status lines per article, so upload throughput must
//! be measured on this side. Speed is a trailing 10-second window; the average runs from the first byte.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const WINDOW: usize = 10;

/// Point-in-time view of bytes handed to the pool.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UploadSnapshot {
    pub bytes_uploaded: i64,
    /// Bytes per second over the trailing window; 0 when idle.
    pub speed: f64,
    /// Bytes per second since the first recorded byte.
    pub avg_speed: f64,
}

/// Source of "now" in seconds since the Unix epoch.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Default)]
struct State {
    total: i64,
    first: Option<f64>,
    buckets: [i64; WINDOW],
    /// Unix second of `buckets[epoch % WINDOW]`.
    epoch: i64,
}

pub struct UploadMeter {
    state: Mutex<State>,
    now: Clock,
}

fn system_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl Default for UploadMeter {
    fn default() -> Self {
        Self::with_clock(Arc::new(system_now))
    }
}

impl UploadMeter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_clock(now: Clock) -> Self {
        Self {
            state: Mutex::new(State::default()),
            now,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Rolls the ring forward to second `sec`, zeroing buckets that left the window.
    fn advance(s: &mut State, sec: i64) {
        if s.epoch == 0 {
            s.epoch = sec;
            return;
        }
        let gap = sec - s.epoch;
        if gap <= 0 {
            return;
        }
        if gap >= WINDOW as i64 {
            s.buckets = [0; WINDOW];
        } else {
            for i in 1..=gap {
                s.buckets[((s.epoch + i) % WINDOW as i64) as usize] = 0;
            }
        }
        s.epoch = sec;
    }

    /// Adds `n` payload bytes at the current instant; non-positive `n` is ignored.
    pub fn record(&self, n: i64) {
        if n <= 0 {
            return;
        }
        let t = (self.now)();
        let mut s = self.lock();
        s.first.get_or_insert(t);
        Self::advance(&mut s, t as i64);
        s.total += n;
        let idx = (s.epoch % WINDOW as i64) as usize;
        s.buckets[idx] += n;
    }

    pub fn snapshot(&self) -> UploadSnapshot {
        let t = (self.now)();
        let mut s = self.lock();
        Self::advance(&mut s, t as i64);
        let window: i64 = s.buckets.iter().sum();
        let mut snap = UploadSnapshot {
            bytes_uploaded: s.total,
            speed: window as f64 / WINDOW as f64,
            avg_speed: 0.0,
        };
        if let Some(first) = s.first {
            let elapsed = t - first;
            if elapsed > 0.0 {
                snap.avg_speed = s.total as f64 / elapsed;
            }
        }
        snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> (Arc<Mutex<f64>>, UploadMeter) {
        let t = Arc::new(Mutex::new(1_000_000.0));
        let t2 = t.clone();
        (
            t,
            UploadMeter::with_clock(Arc::new(move || *t2.lock().unwrap())),
        )
    }

    #[test]
    fn totals_recorded_bytes() {
        let m = UploadMeter::new();
        m.record(100);
        m.record(50);
        m.record(-5);
        assert_eq!(m.snapshot().bytes_uploaded, 150);
    }

    #[test]
    fn speed_reflects_the_recent_window_only() {
        let (t, m) = fake();
        for _ in 0..5 {
            m.record(1 << 20);
            *t.lock().unwrap() += 1.0;
        }
        let speed = m.snapshot().speed;
        assert!(
            (speed - f64::from(1 << 20)).abs() <= f64::from(1 << 20) * 0.6,
            "{speed}"
        );
        *t.lock().unwrap() += 20.0;
        assert_eq!(
            m.snapshot().speed,
            0.0,
            "speed decays to 0 once the window has passed"
        );
        assert_eq!(
            m.snapshot().bytes_uploaded,
            5 << 20,
            "totals survive the window"
        );
    }

    #[test]
    fn average_runs_from_the_first_byte() {
        let (t, m) = fake();
        m.record(1000);
        *t.lock().unwrap() += 10.0;
        m.record(1000);
        assert!((m.snapshot().avg_speed - 200.0).abs() < 1.0);
    }
}
