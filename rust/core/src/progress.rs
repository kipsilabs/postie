//! Port of `internal/progress`: per-job progress tracking for uploads, PAR2 generation and checks.
//!
//! The Go package wraps `schollz/progressbar` (which also draws a terminal bar). The state the
//! UI reads is reproduced here exactly (bar semantics: the count only advances while below the
//! maximum, `Finish` jumps to the maximum); drawing a terminal bar belongs to the CLI entry point.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use uuid::Uuid;

/// What a progress indicator tracks. Serializes to the Go string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProgressType {
    #[serde(rename = "uploading")]
    Uploading,
    #[serde(rename = "par2_generation")]
    Par2Generation,
    #[serde(rename = "checking")]
    Checking,
}

/// Snapshot sent to the frontend. JSON field names are the Go struct field names.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ProgressState {
    pub max: i64,
    pub current_num: i64,
    pub current_percent: f64,
    pub current_bytes: f64,
    pub seconds_since: f64,
    pub seconds_left: f64,
    #[serde(rename = "KBsPerSecond")]
    pub kbs_per_second: f64,
    pub description: String,
    pub r#type: ProgressType,
    pub is_started: bool,
    pub is_waiting: bool,
    pub wait_seconds_remaining: f64,
    pub is_paused: bool,
}

/// Source of "now"; tests substitute a controllable clock.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct Inner {
    current: i64,
    current_bytes: f64,
    finished: bool,
    paused: bool,
    wait_deadline: Option<Instant>,
    bytes: i64,
    first_byte_at: Option<Instant>,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

/// One progress indicator.
pub struct Progress {
    id: Uuid,
    name: String,
    ptype: ProgressType,
    total: i64,
    start: Instant,
    clock: Clock,
    inner: Mutex<Inner>,
}

fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

impl Progress {
    fn new(id: Uuid, name: String, ptype: ProgressType, total: i64, clock: Clock) -> Self {
        // total is never 0: avoids division by zero
        let total = if total <= 0 { 1 } else { total };
        let start = clock();
        Self {
            id,
            name,
            ptype,
            total,
            start,
            clock,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn progress_type(&self) -> ProgressType {
        self.ptype
    }
    pub fn total(&self) -> i64 {
        self.total
    }
    pub fn start_time(&self) -> Instant {
        self.start
    }

    /// Time spent transferring: from the first byte, minus paused intervals.
    fn active_seconds(i: &Inner, now: Instant) -> f64 {
        let Some(first) = i.first_byte_at else {
            return 0.0;
        };
        let mut active =
            now.saturating_duration_since(first).as_secs_f64() - i.paused_total.as_secs_f64();
        if let Some(p) = i.paused_at {
            active -= now.saturating_duration_since(p).as_secs_f64();
        }
        active
    }

    /// Records `processed` more units. Ignored once the bar is finished.
    pub fn update_progress(&self, processed: i64) {
        let mut i = self.lock();
        if i.finished {
            return;
        }
        if i.first_byte_at.is_none() {
            let now = (self.clock)();
            i.first_byte_at = Some(now);
            i.paused_total = Duration::ZERO;
            if i.paused {
                i.paused_at = Some(now);
            }
        }
        i.bytes += processed;
        // bar semantics: the count only advances while below the maximum
        if i.current < self.total {
            i.current += processed;
        }
        i.current_bytes += processed as f64;
        if i.current >= self.total {
            i.finished = true;
        }
    }

    /// Jumps to the maximum and marks the bar complete.
    pub fn finish(&self) {
        let mut i = self.lock();
        i.current = self.total;
        i.current_bytes = self.total as f64;
        i.finished = true;
    }

    pub fn state(&self) -> ProgressState {
        let i = self.lock();
        let now = (self.clock)();
        let seconds_since = finite_or_zero(now.saturating_duration_since(self.start).as_secs_f64());
        let active = Self::active_seconds(&i, now);
        let (mut kbs, mut left) = (0.0, 0.0);
        if i.bytes > 0 && active > 0.0 {
            kbs = i.bytes as f64 / 1024.0 / active;
            let remaining = self.total - i.bytes;
            if remaining > 0 {
                left = remaining as f64 / (kbs * 1024.0);
            }
        }
        let (mut waiting, mut wait_left) = (false, 0.0);
        if let Some(d) = i.wait_deadline {
            let r = d.saturating_duration_since(now).as_secs_f64();
            if r > 0.0 {
                waiting = true;
                wait_left = r;
            }
        }
        ProgressState {
            max: self.total,
            current_num: i.current,
            current_percent: finite_or_zero(i.current as f64 / self.total as f64),
            current_bytes: finite_or_zero(i.current_bytes),
            seconds_since,
            seconds_left: left,
            kbs_per_second: kbs,
            description: self.name.clone(),
            r#type: self.ptype,
            is_started: i.current > 0,
            is_waiting: waiting,
            wait_seconds_remaining: wait_left,
            is_paused: i.paused,
        }
    }

    pub fn current(&self) -> i64 {
        self.lock().current
    }

    pub fn percentage(&self) -> f64 {
        finite_or_zero(self.lock().current as f64 / self.total as f64)
    }

    pub fn is_complete(&self) -> bool {
        self.lock().finished
    }

    /// Elapsed time truncated to whole seconds, as the Go getter does.
    pub fn elapsed(&self) -> Duration {
        let secs = ((self.clock)().saturating_duration_since(self.start)).as_secs_f64();
        Duration::from_secs(secs as u64)
    }

    pub fn set_paused(&self, paused: bool) {
        let mut i = self.lock();
        if paused == i.paused {
            return;
        }
        i.paused = paused;
        let now = (self.clock)();
        if paused {
            i.paused_at = Some(now);
            return;
        }
        if let Some(p) = i.paused_at.take() {
            i.paused_total += now.saturating_duration_since(p);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.lock().paused
    }

    pub fn set_wait_deadline(&self, deadline: Option<Instant>) {
        self.lock().wait_deadline = deadline;
    }
}

/// All progress indicators of one job.
pub struct JobProgress {
    job_id: String,
    clock: Clock,
    active: RwLock<HashMap<Uuid, Arc<Progress>>>,
}

impl JobProgress {
    pub fn new(job_id: impl Into<String>) -> Self {
        Self::with_clock(job_id, Arc::new(Instant::now))
    }

    pub fn with_clock(job_id: impl Into<String>, clock: Clock) -> Self {
        Self {
            job_id: job_id.into(),
            clock,
            active: RwLock::default(),
        }
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn add_progress(
        &self,
        id: Uuid,
        name: &str,
        ptype: ProgressType,
        total: i64,
    ) -> Arc<Progress> {
        let p = Arc::new(Progress::new(
            id,
            name.to_string(),
            ptype,
            total,
            self.clock.clone(),
        ));
        self.active
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, p.clone());
        p
    }

    /// Finishes and removes the indicator; a no-op for unknown ids.
    pub fn finish_progress(&self, id: Uuid) {
        if let Some(p) = self
            .active
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        {
            p.finish();
        }
    }

    pub fn get_progress(&self, id: Uuid) -> Option<Arc<Progress>> {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }

    pub fn get_all_progress(&self) -> HashMap<Uuid, Arc<Progress>> {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// States sorted by current count (descending), then description (ascending).
    pub fn get_all_progress_state(&self) -> Vec<ProgressState> {
        let mut out: Vec<ProgressState> = self
            .active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|p| p.state())
            .collect();
        out.sort_by(|a, b| {
            b.current_num
                .cmp(&a.current_num)
                .then_with(|| a.description.cmp(&b.description))
        });
        out
    }

    /// Pauses or resumes every indicator except PAR2 generation, which cannot be paused.
    pub fn set_all_paused(&self, paused: bool) {
        for p in self
            .active
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            if p.progress_type() != ProgressType::Par2Generation {
                p.set_paused(paused);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Controllable clock.
    struct Fake(Arc<Mutex<Instant>>);
    impl Fake {
        fn new() -> (Self, Clock) {
            let t = Arc::new(Mutex::new(Instant::now()));
            let t2 = t.clone();
            (Self(t), Arc::new(move || *t2.lock().unwrap()))
        }
        fn advance(&self, secs: u64) {
            *self.0.lock().unwrap() += Duration::from_secs(secs);
        }
    }

    fn setup(total: i64) -> (Fake, Arc<Progress>) {
        let (fake, clock) = Fake::new();
        let job = JobProgress::with_clock("job", clock);
        let p = job.add_progress(Uuid::new_v4(), "file.bin", ProgressType::Uploading, total);
        (fake, p)
    }

    fn close(name: &str, got: f64, want: f64) {
        assert!((got - want).abs() <= 0.01, "{name} = {got}, want {want}");
    }

    #[test]
    fn speed_excludes_time_waiting_for_first_byte() {
        let (c, p) = setup(4096);
        c.advance(30);
        p.update_progress(1024);
        c.advance(1);
        p.update_progress(1024);
        close("KBsPerSecond", p.state().kbs_per_second, 2.0);
    }

    #[test]
    fn speed_excludes_paused_time() {
        let (c, p) = setup(4096);
        p.update_progress(1024);
        c.advance(1);
        p.set_paused(true);
        c.advance(10);
        p.set_paused(false);
        c.advance(1);
        p.update_progress(1024);
        close("KBsPerSecond", p.state().kbs_per_second, 1.0);
    }

    #[test]
    fn seconds_left_uses_active_speed() {
        let (c, p) = setup(4096);
        c.advance(30);
        p.update_progress(1024);
        c.advance(1);
        p.update_progress(1024);
        close("SecondsLeft", p.state().seconds_left, 1.0);
    }

    #[test]
    fn speed_is_zero_before_first_byte() {
        let (c, p) = setup(4096);
        c.advance(5);
        let s = p.state();
        close("KBsPerSecond", s.kbs_per_second, 0.0);
        close("SecondsLeft", s.seconds_left, 0.0);
    }

    #[test]
    fn pause_before_first_byte_does_not_skew_speed() {
        let (c, p) = setup(4096);
        p.set_paused(true);
        c.advance(10);
        p.set_paused(false);
        c.advance(5);
        p.update_progress(1024);
        c.advance(1);
        p.update_progress(1024);
        close("KBsPerSecond", p.state().kbs_per_second, 2.0);
    }

    #[test]
    fn bar_semantics_overshoot_then_ignore() {
        let (_c, p) = setup(10);
        p.update_progress(8);
        assert!(!p.is_complete());
        p.update_progress(5); // overshoots once, like the Go bar
        assert_eq!(p.current(), 13);
        assert!(p.is_complete());
        p.update_progress(5); // finished: ignored
        assert_eq!(p.current(), 13);
    }

    #[test]
    fn zero_total_becomes_one_and_finish_jumps_to_max() {
        let (_c, p) = setup(0);
        assert_eq!(p.total(), 1);
        p.finish();
        let s = p.state();
        assert_eq!(
            (s.max, s.current_num, s.current_percent, s.current_bytes),
            (1, 1, 1.0, 1.0)
        );
        assert!(p.is_complete());
    }

    #[test]
    fn states_sorted_and_par2_not_pausable() {
        let (_c, clock) = Fake::new();
        let job = JobProgress::with_clock("j", clock);
        let a = job.add_progress(Uuid::new_v4(), "b", ProgressType::Uploading, 100);
        let b = job.add_progress(Uuid::new_v4(), "a", ProgressType::Uploading, 100);
        let par = job.add_progress(Uuid::new_v4(), "par", ProgressType::Par2Generation, 100);
        a.update_progress(10);
        b.update_progress(10);
        par.update_progress(50);
        let order: Vec<_> = job
            .get_all_progress_state()
            .into_iter()
            .map(|s| s.description)
            .collect();
        assert_eq!(order, ["par", "a", "b"]);
        job.set_all_paused(true);
        assert!(a.is_paused() && b.is_paused() && !par.is_paused());
        job.finish_progress(a.id());
        assert!(job.get_progress(a.id()).is_none() && a.is_complete());
    }

    #[test]
    fn json_uses_go_field_names() {
        let (_c, p) = setup(10);
        let v = serde_json::to_value(p.state()).unwrap();
        for k in [
            "Max",
            "CurrentNum",
            "CurrentPercent",
            "CurrentBytes",
            "SecondsSince",
            "SecondsLeft",
            "KBsPerSecond",
            "Description",
            "Type",
            "IsStarted",
            "IsWaiting",
            "WaitSecondsRemaining",
            "IsPaused",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert_eq!(v["Type"], "uploading");
    }

    #[test]
    fn wait_deadline_reports_remaining() {
        let (c, p) = setup(10);
        let now = (p.clock)();
        p.set_wait_deadline(Some(now + Duration::from_secs(5)));
        c.advance(2);
        let s = p.state();
        assert!(s.is_waiting);
        close("wait", s.wait_seconds_remaining, 3.0);
        c.advance(10);
        assert!(!p.state().is_waiting);
    }
}
