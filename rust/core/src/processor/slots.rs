//! Port of `slots.go`: runs tasks on up to a fixed number of concurrent slots. Unlike a batch (start N,
//! wait for all N), a freed slot is refilled immediately, so one slow task never idles the others.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct SlotRunner {
    active: Arc<AtomicUsize>,
    /// Signalled each time a task finishes so the dispatcher can refill without waiting for its tick.
    wake: Arc<Notify>,
}

/// Releases the slot (and wakes the dispatcher) even if the task panics.
struct Slot(SlotRunner);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.wake.notify_one();
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
        let r = release.clone();
        assert_eq!(
            s.fill(2, move || {
                let r = r.clone();
                async move { r.notified().await }
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
        tokio::time::timeout(Duration::from_secs(2), s.woken())
            .await
            .unwrap();
        assert_eq!(s.active(), 0);
        assert_eq!(s.fill(1, || async {}), 1);
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
