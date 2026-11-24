//! Port of `internal/poster/uploadmeter.go`: counts article payload bytes handed to the NNTP pool.
//! The pool only reports a few hundred bytes of status lines per article, so upload throughput must
//! be measured on this side. Speed is a short trailing window (so a burst reads at its real rate and
//! falls to 0 soon after it ends); the average covers only seconds in which bytes were sent, so idle
//! time between uploads does not drag it down.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds in the speed window (one bucket per second).
const WINDOW: usize = 3;

/// Point-in-time view of bytes handed to the pool.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UploadSnapshot {
    pub bytes_uploaded: i64,
    /// Bytes per second over the trailing window; 0 when idle.
    pub speed: f64,
    /// Bytes per second over the seconds in which bytes were sent.
    pub avg_speed: f64,
}

/// Floor for time spans, so a first byte does not read as an absurd rate.
const MIN_SPAN_SECS: f64 = 0.5;

/// Source of "now" in seconds since the Unix epoch.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Default)]
struct State {
    total: i64,
    first: Option<f64>,
    /// Instant of the latest recorded byte, and how many distinct seconds have carried bytes.
    last: f64,
    active_secs: i64,
    last_active_sec: Option<i64>,
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
        s.last = t;
        let sec = t as i64;
        if s.last_active_sec != Some(sec) {
            s.last_active_sec = Some(sec);
            s.active_secs += 1;
        }
        Self::advance(&mut s, sec);
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
            ..Default::default()
        };
        let Some(first) = s.first else { return snap };
        if window > 0 {
            // time the window really spans: the full past seconds plus the elapsed part of the
            // current one, but never more than the traffic has existed
            let span = ((WINDOW - 1) as f64 + t.fract()).min(t - first);
            snap.speed = window as f64 / span.max(MIN_SPAN_SECS);
        }
        // seconds with traffic, the last one counted only as far as its last byte, so a sub-second
        // burst is not diluted to a full second
        let active = (s.active_secs - 1) as f64 + s.last.fract().max(MIN_SPAN_SECS);
        snap.avg_speed = s.total as f64 / active.max(MIN_SPAN_SECS);
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

    /// A short burst must read close to its real rate, not be diluted over a long fixed window.
    #[test]
    fn a_short_burst_reads_close_to_its_real_rate() {
        let (t, m) = fake();
        // 50 MiB/s for 2 seconds, recorded 10 times per second
        for _ in 0..20 {
            m.record(5 << 20);
            *t.lock().unwrap() += 0.1;
        }
        let speed = m.snapshot().speed;
        let real = f64::from(50 << 20);
        assert!(
            (speed - real).abs() <= real * 0.35,
            "speed {speed} should be near {real}"
        );
    }

    #[test]
    fn speed_drops_to_zero_shortly_after_the_last_byte() {
        let (t, m) = fake();
        for _ in 0..20 {
            m.record(5 << 20);
            *t.lock().unwrap() += 0.1;
        }
        *t.lock().unwrap() += WINDOW as f64 + 0.01;
        assert_eq!(m.snapshot().speed, 0.0);
    }

    /// The average covers only seconds in which bytes were sent, so idling does not drag it down.
    #[test]
    fn average_ignores_idle_time() {
        let (t, m) = fake();
        for _ in 0..20 {
            m.record(5 << 20);
            *t.lock().unwrap() += 0.1;
        }
        let during = m.snapshot().avg_speed;
        *t.lock().unwrap() += 600.0;
        let after_idle = m.snapshot().avg_speed;
        assert_eq!(during, after_idle, "idle time leaves the average unchanged");
        let real = f64::from(50 << 20);
        assert!(
            (after_idle - real).abs() <= real * 0.35,
            "average {after_idle} should be near {real}"
        );
    }
}
