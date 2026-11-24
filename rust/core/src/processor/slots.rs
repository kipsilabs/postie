//! Port of `slots.go`: runs tasks on up to a fixed number of concurrent slots. Unlike a batch (start N,
//! wait for all N), a freed slot is refilled immediately, so one slow task never idles the others.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct SlotRunner {
    active: Arc<AtomicUsize>,
    /// Signalled (via [`SlotRunner::signal`]) when a task did real work, so the dispatcher refills
    /// without waiting for its tick.
    wake: Arc<Notify>,
}

/// Releases the slot even if the task panics. It deliberately does NOT wake the dispatcher: with an
/// idle queue every task returns at once, and wake-on-finish turns refilling into a busy loop of empty
/// queue polls. Tasks that processed something call `signal` themselves.
struct Slot(SlotRunner);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SlotRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Starts `task` on every free slot up to `limit` and returns how many it started. The slot is
    /// claimed before the task is spawned, so overlapping `fill` calls can never exceed `limit`.
    pub fn fill<F, Fut>(&self, limit: usize, mut task: F) -> usize
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut started = 0;
        loop {
            let cur = self.active.load(Ordering::SeqCst);
            if cur >= limit {
                return started;
            }
            if self
                .active
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                continue;
            }
            started += 1;
            let slot = Slot(self.clone());
            let fut = task();
            tokio::spawn(async move {
                let _slot = slot;
                fut.await;
            });
        }
    }

    /// Wakes the dispatcher without blocking (at most one wake-up is remembered).
    pub fn signal(&self) {
        self.wake.notify_one();
    }

    /// Resolves when a task finished or `signal` was called.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn never_exceeds_the_limit_even_with_overlapping_fills() {
        let s = SlotRunner::new();
        let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut hs = vec![];
        for _ in 0..8 {
            let (s, r, p) = (s.clone(), running.clone(), peak.clone());
            hs.push(tokio::spawn(async move {
                for _ in 0..50 {
                    s.fill(3, || {
                        let (r, p) = (r.clone(), p.clone());
                        async move {
                            let now = r.fetch_add(1, Ordering::SeqCst) + 1;
                            p.fetch_max(now, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(1)).await;
                            r.fetch_sub(1, Ordering::SeqCst);
                        }
                    });
                    tokio::task::yield_now().await;
                }
            }));
        }
        for h in hs {
            h.await.unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "peak {}",
            peak.load(Ordering::SeqCst)
        );
        assert!(peak.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn a_freed_slot_is_refilled_without_waiting_for_the_others() {
        let s = SlotRunner::new();
        let release = Arc::new(Notify::new());
        let (r, s2) = (release.clone(), s.clone());
        assert_eq!(
            s.fill(2, move || {
                let (r, s2) = (r.clone(), s2.clone());
                async move {
                    r.notified().await;
                    s2.signal(); // a task that did work wakes the dispatcher (WakeOnDrop)
                }
            }),
            2
        );
        assert_eq!(s.fill(2, || async {}), 0, "full");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), s.woken())
            .await
            .expect("woken when a task finishes");
        assert_eq!(s.fill(2, || async {}), 1, "exactly the freed slot");
        release.notify_one();
    }

    #[tokio::test]
    async fn a_panicking_task_releases_its_slot() {
        let s = SlotRunner::new();
        s.fill(1, || async { panic!("boom") });
        tokio::time::timeout(Duration::from_secs(2), async {
            while s.active() > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("slot released after the panic");
        assert_eq!(s.fill(1, || async {}), 1);
    }

    /// An idle queue makes every task return at once; if finishing woke the dispatcher, it would
    /// refill, finish and wake again forever (a 100% CPU spin of empty queue polls).
    #[tokio::test]
    async fn a_task_finishing_does_not_wake_the_dispatcher_by_itself() {
        let s = SlotRunner::new();
        assert_eq!(s.fill(2, || async {}), 2);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(s.active(), 0, "slots are released");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), s.woken())
                .await
                .is_err(),
            "no wake-up without an explicit signal"
        );
    }

    #[tokio::test]
    async fn signal_wakes_the_dispatcher() {
        let s = SlotRunner::new();
        s.signal();
        tokio::time::timeout(Duration::from_millis(200), s.woken())
            .await
            .expect("stored permit");
    }
}
