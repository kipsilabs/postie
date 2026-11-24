//! Nyuu-style checking during upload: every posted article is STATed once the propagation delay
//! after *its own* post has passed, while the rest of the upload is still running. Confirmed
//! Message-IDs are remembered so the file's verification sweep can skip them; by the time a large
//! upload finishes, almost every article is already confirmed and the sweep has little left to do.
//!
//! The sweep can start as soon as an upload ends: an article still waiting here is awaited
//! ([`EarlyChecker::settle`]) rather than STATed before its propagation delay has passed.
//!
//! This is purely an accelerator. A miss, an error, a dropped notification or a restart only means
//! the article is not pre-confirmed, and the durable sweep checks it as before. Nothing here
//! writes to the database or re-posts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::Stater;
use crate::poster::PostObserver;
use crate::progress::Progress;

/// Posted articles waiting for their check. Past this, notifications are dropped (never block the
/// upload) and those articles are left for the sweep.
const QUEUE_CAPACITY: usize = 100_000;
/// Upper bound on remembered confirmations (about 100 MB of Message-IDs at worst).
const MAX_CONFIRMED: usize = 1_000_000;
/// Confirmations no sweep consumed (a cancelled or discarded job) are forgotten after this long.
const CONFIRMED_TTL: Duration = Duration::from_secs(12 * 3600);
const PRUNE_EVERY: Duration = Duration::from_secs(60);
/// How long past an article's due time the sweep waits for its check before STATing it itself
/// (a stalled STAT, or a checker that stopped).
const SETTLE_GRACE: Duration = Duration::from_secs(60);

struct Posted {
    id: String,
    due: Instant,
    /// The job's "verifying" row and the article's bytes, credited on confirmation.
    row: Option<(Arc<Progress>, i64)>,
}

fn normalize(id: &str) -> &str {
    id.trim_matches(['<', '>'])
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct EarlyChecker {
    stater: Arc<dyn Stater>,
    delay: Duration,
    tx: mpsc::Sender<Posted>,
    rx: Mutex<Option<mpsc::Receiver<Posted>>>,
    confirmed: Mutex<HashMap<String, Instant>>,
    /// Articles queued or being checked, with the time their check is due.
    pending: Mutex<HashMap<String, Instant>>,
    /// Signalled whenever a pending article's check finishes (or the checker stops).
    settled: tokio::sync::Notify,
}

impl EarlyChecker {
    /// `delay` is the propagation delay waited after each post before its STAT.
    pub fn new(stater: Arc<dyn Stater>, delay: Duration) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        Self {
            stater,
            delay,
            tx,
            rx: Mutex::new(Some(rx)),
            confirmed: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            settled: tokio::sync::Notify::new(),
        }
    }

    /// Whether a STAT already confirmed this article; consumes the confirmation.
    pub fn take_confirmed(&self, message_id: &str) -> bool {
        lock(&self.confirmed)
            .remove(normalize(message_id))
            .is_some()
    }

    /// Whether this article is still waiting for (or in) its check here.
    pub fn is_pending(&self, message_id: &str) -> bool {
        lock(&self.pending).contains_key(normalize(message_id))
    }

    /// Waits for a pending article's check, then reports (and consumes) whether it confirmed the
    /// article. Gives up `SETTLE_GRACE` past the check's due time, so a stalled check or a stopped
    /// checker only costs the sweep its own STAT.
    pub async fn settle(&self, message_id: &str) -> bool {
        let id = normalize(message_id);
        loop {
            let notified = self.settled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let Some(due) = lock(&self.pending).get(id).copied() else {
                return self.take_confirmed(id);
            };
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(due + SETTLE_GRACE) => return self.take_confirmed(id),
            }
        }
    }

    fn finish(&self, id: &str) {
        lock(&self.pending).remove(id);
        self.settled.notify_waiters();
    }

    fn confirm(&self, id: String) {
        let mut m = lock(&self.confirmed);
        if m.len() < MAX_CONFIRMED {
            m.insert(id, Instant::now());
        }
    }

    fn prune(&self) {
        let now = Instant::now();
        lock(&self.confirmed).retain(|_, at| now.duration_since(*at) < CONFIRMED_TTL);
    }

    /// Checks posted articles until `cancel` fires. Runs at most once (later calls return at once).
    /// Due checks are issued in posting order, up to the stater's window at a time.
    pub async fn run(&self, cancel: &CancellationToken) {
        let Some(rx) = lock(&self.rx).take() else {
            return;
        };
        self.check_posted(rx, cancel).await;
        // nothing pending will be checked any more: let waiting sweeps STAT for themselves
        lock(&self.pending).clear();
        self.settled.notify_waiters();
    }

    async fn check_posted(&self, mut rx: mpsc::Receiver<Posted>, cancel: &CancellationToken) {
        let window = self.stater.window().max(1);
        let mut inflight = FuturesUnordered::new();
        let mut head: Option<Posted> = None;
        let mut next_prune = Instant::now() + PRUNE_EVERY;
        loop {
            let room = inflight.len() < window;
            let due = head.as_ref().map(|p| p.due);
            tokio::select! {
                () = cancel.cancelled() => return,
                Some((p, present)) = inflight.next(), if !inflight.is_empty() => {
                    let (p, present): (Posted, bool) = (p, present);
                    if present {
                        if let Some((row, size)) = &p.row {
                            row.update_progress(*size);
                        }
                        self.confirm(p.id.clone());
                    }
                    self.finish(&p.id);
                }
                () = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)),
                    if room && due.is_some() =>
                {
                    if let Some(p) = head.take() {
                        let stater = &self.stater;
                        inflight.push(async move {
                            let present = matches!(stater.stat(&p.id).await, Ok(false));
                            (p, present)
                        });
                    }
                }
                p = rx.recv(), if head.is_none() => match p {
                    Some(p) => head = Some(p),
                    None => return, // every sender is gone: nothing more will be posted
                },
            }
            if Instant::now() >= next_prune {
                self.prune();
                next_prune = Instant::now() + PRUNE_EVERY;
            }
        }
    }
}

impl PostObserver for EarlyChecker {
    fn posted(&self, message_id: &str, size: i64, row: Option<&Arc<Progress>>) {
        let due = Instant::now() + self.delay;
        let id = normalize(message_id).to_string();
        // pending before it is queued, so the checker never finishes an id not yet marked
        lock(&self.pending).insert(id.clone(), due);
        let posted = Posted {
            id: id.clone(),
            due,
            row: row.map(|r| (r.clone(), size)),
        };
        // full queue (or a stopped checker): drop it, the sweep checks this article as usual
        if self.tx.try_send(posted).is_err() {
            self.finish(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::poster::BoxFut;

    /// Ids starting with `x` are missing; counts STATs.
    #[derive(Default)]
    struct Fake(AtomicUsize);
    impl Stater for Fake {
        fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, String>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(id.starts_with('x'))
            })
        }
        fn stat_batch<'a>(
            &'a self,
            _ids: &'a [String],
        ) -> BoxFut<'a, Result<HashSet<String>, String>> {
            Box::pin(async { Ok(HashSet::new()) })
        }
    }

    async fn settle(ec: &EarlyChecker, cancel: &CancellationToken, wait: Duration) {
        tokio::select! {
            () = ec.run(cancel) => {}
            () = tokio::time::sleep(wait) => {}
        }
    }

    #[tokio::test(start_paused = true)]
    async fn posted_articles_are_confirmed_only_after_the_delay() {
        let fake = Arc::new(Fake::default());
        let ec = EarlyChecker::new(fake.clone(), Duration::from_secs(5));
        ec.posted("<a@x>", 0, None);
        ec.posted("x-missing@x", 0, None);
        let cancel = CancellationToken::new();
        let run = ec.run(&cancel);
        tokio::pin!(run);
        tokio::select! {
            () = &mut run => unreachable!(),
            () = tokio::time::sleep(Duration::from_secs(4)) => {}
        }
        assert_eq!(fake.0.load(Ordering::SeqCst), 0, "checked before the delay");
        tokio::select! {
            () = &mut run => unreachable!(),
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        assert_eq!(fake.0.load(Ordering::SeqCst), 2);
        assert!(ec.take_confirmed("a@x"), "angle brackets are ignored");
        assert!(
            !ec.take_confirmed("<a@x>"),
            "a confirmation is consumed once"
        );
        assert!(
            !ec.take_confirmed("x-missing@x"),
            "a miss is left to the sweep"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn confirmed_articles_advance_the_verifying_row() {
        let progress = crate::progress::JobProgress::new("j");
        let row = progress.add_progress(
            uuid::Uuid::new_v4(),
            "a.bin (verifying)",
            crate::progress::ProgressType::Checking,
            1000,
        );
        let ec = EarlyChecker::new(Arc::new(Fake::default()), Duration::from_secs(1));
        ec.posted("<a@x>", 300, Some(&row));
        ec.posted("<x-missing@x>", 500, Some(&row));
        ec.posted("<b@x>", 200, None);
        settle(&ec, &CancellationToken::new(), Duration::from_secs(2)).await;
        assert_eq!(row.state().current_num, 300, "only confirmed bytes count");
    }

    #[tokio::test(start_paused = true)]
    async fn run_stops_on_cancel_and_only_runs_once() {
        let ec = EarlyChecker::new(Arc::new(Fake::default()), Duration::from_secs(1));
        let cancel = CancellationToken::new();
        cancel.cancel();
        ec.run(&cancel).await;
        settle(&ec, &CancellationToken::new(), Duration::from_secs(1)).await;
        ec.posted("<late@x>", 0, None);
        settle(&ec, &CancellationToken::new(), Duration::from_secs(5)).await;
        assert!(!ec.take_confirmed("late@x"), "a second run must not start");
    }

    #[tokio::test(start_paused = true)]
    async fn settle_waits_for_a_pending_check() {
        let ec = Arc::new(EarlyChecker::new(
            Arc::new(Fake::default()),
            Duration::from_secs(5),
        ));
        ec.posted("<a@x>", 0, None);
        ec.posted("<x-missing@x>", 0, None);
        assert!(ec.is_pending("a@x") && ec.is_pending("<x-missing@x>"));
        assert!(!ec.is_pending("never-posted@x"));
        let cancel = CancellationToken::new();
        let (a, missing) = tokio::select! {
            () = ec.run(&cancel) => unreachable!(),
            r = async { tokio::join!(ec.settle("a@x"), ec.settle("x-missing@x")) } => r,
        };
        assert!(a, "confirmed once its check ran");
        assert!(!missing, "a miss is left to the sweep");
        assert!(!ec.is_pending("a@x"));
        assert!(!ec.settle("never-posted@x").await, "nothing to wait for");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_checker_releases_waiting_sweeps() {
        let ec = EarlyChecker::new(Arc::new(Fake::default()), Duration::from_secs(5));
        ec.posted("<a@x>", 0, None);
        let cancel = CancellationToken::new();
        cancel.cancel();
        ec.run(&cancel).await;
        assert!(!ec.is_pending("a@x"));
        ec.posted("<b@x>", 0, None);
        assert!(!ec.is_pending("b@x"), "nothing will check it any more");
    }

    #[tokio::test(start_paused = true)]
    async fn settle_gives_up_on_a_checker_that_never_runs() {
        let ec = EarlyChecker::new(Arc::new(Fake::default()), Duration::from_secs(5));
        ec.posted("<a@x>", 0, None);
        let start = Instant::now();
        assert!(!ec.settle("a@x").await);
        assert!(start.elapsed() >= Duration::from_secs(5) + SETTLE_GRACE);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_confirmations_are_pruned() {
        let ec = EarlyChecker::new(Arc::new(Fake::default()), Duration::ZERO);
        ec.confirm("old@x".into());
        tokio::time::advance(CONFIRMED_TTL + Duration::from_secs(1)).await;
        ec.confirm("new@x".into());
        ec.prune();
        assert!(!ec.take_confirmed("old@x"));
        assert!(ec.take_confirmed("new@x"));
    }
}
