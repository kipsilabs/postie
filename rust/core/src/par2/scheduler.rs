//! Port of `internal/par2/scheduler.go`: bounds how many PAR2 operations run at once process-wide, so
//! the configured PAR2 memory limit applies per active job instead of being multiplied across jobs.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::executor::{ExecError, Par2Executor};
use super::existing::Par2Result;
use crate::fileinfo::FileInfo;
use crate::poster::BoxFut;

pub struct Scheduler {
    sem: Semaphore,
    capacity: usize,
    active: AtomicI64,
    queued: AtomicI64,
}

impl Scheduler {
    /// Allows at most `max_concurrent_jobs` operations at once (values below 1 mean 1).
    pub fn new(max_concurrent_jobs: i64) -> Self {
        let capacity = usize::try_from(max_concurrent_jobs).unwrap_or(1).max(1);
        Self {
            sem: Semaphore::new(capacity),
            capacity,
            active: AtomicI64::new(0),
            queued: AtomicI64::new(0),
        }
    }

    /// Waits for a slot, runs `fut`, releases the slot. Cancelled while waiting: returns `Cancelled`
    /// without polling `fut`.
    pub async fn run<T>(
        &self,
        cancel: &CancellationToken,
        fut: impl Future<Output = Result<T, ExecError>>,
    ) -> Result<T, ExecError> {
        self.queued.fetch_add(1, Ordering::SeqCst);
        let permit = tokio::select! {
            () = cancel.cancelled() => {
                self.queued.fetch_sub(1, Ordering::SeqCst);
                return Err(ExecError::Cancelled);
            }
            p = self.sem.acquire() => p,
        };
        self.queued.fetch_sub(1, Ordering::SeqCst);
        let Ok(_permit) = permit else {
            return Err(ExecError::Msg("scheduler closed".into()));
        };
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveGuard(&self.active);
        fut.await
    }

    pub fn active(&self) -> i64 {
        self.active.load(Ordering::SeqCst)
    }

    pub fn queued(&self) -> i64 {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

struct ActiveGuard<'a>(&'a AtomicI64);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Wraps an executor so every operation runs through a shared [`Scheduler`]; the wrapped executor keeps
/// its per-job progress and configuration, only execution is gated.
pub struct ScheduledExecutor {
    inner: Arc<dyn Par2Executor>,
    sched: Arc<Scheduler>,
}

impl ScheduledExecutor {
    /// With no scheduler the inner executor is returned unchanged.
    pub fn wrap(
        inner: Arc<dyn Par2Executor>,
        sched: Option<Arc<Scheduler>>,
    ) -> Arc<dyn Par2Executor> {
        match sched {
            Some(sched) => Arc::new(Self { inner, sched }),
            None => inner,
        }
    }
}

impl Par2Executor for ScheduledExecutor {
    fn create<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.sched.run(cancel, self.inner.create(cancel, files)))
    }

    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.sched.run(
            cancel,
            self.inner.create_in_directory(cancel, files, output_dir),
        ))
    }

    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
        set_name: &'a str,
        folder_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(
            self.sched.run(
                cancel,
                self.inner
                    .create_set(cancel, files, output_dir, set_name, folder_dir),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[tokio::test]
    async fn bounds_concurrency_and_counts_active_and_queued() {
        let s = Arc::new(Scheduler::new(2));
        let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let cancel = CancellationToken::new();
        let mut hs = vec![];
        for _ in 0..8 {
            let (s, r, p, c) = (s.clone(), running.clone(), peak.clone(), cancel.clone());
            hs.push(tokio::spawn(async move {
                s.run(&c, async {
                    let now = r.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    r.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!((s.active(), s.queued(), s.capacity()), (2, 6, 2));
        for h in hs {
            h.await.unwrap().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!((s.active(), s.queued()), (0, 0));
    }

    #[tokio::test]
    async fn cancelled_while_waiting_never_runs() {
        let s = Arc::new(Scheduler::new(1));
        let hold = CancellationToken::new();
        let s2 = s.clone();
        let h2 = hold.clone();
        let holder = tokio::spawn(async move {
            s2.run(&CancellationToken::new(), async {
                h2.cancelled().await;
                Ok(())
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let waiter_cancel = CancellationToken::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let (s3, c3, r3) = (s.clone(), waiter_cancel.clone(), ran.clone());
        let waiter = tokio::spawn(async move {
            s3.run(&c3, async {
                r3.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        waiter_cancel.cancel();
        assert_eq!(waiter.await.unwrap(), Err(ExecError::Cancelled));
        assert_eq!((ran.load(Ordering::SeqCst), s.queued()), (0, 0));
        hold.cancel();
        holder.await.unwrap().unwrap();
    }

    #[test]
    fn clamps_to_one_and_wrap_without_a_scheduler_returns_the_inner_executor() {
        assert_eq!(Scheduler::new(0).capacity(), 1);
        assert_eq!(Scheduler::new(-5).capacity(), 1);
        struct Nop;
        impl Par2Executor for Nop {
            fn create<'a>(
                &'a self,
                _: &'a CancellationToken,
                _: &'a [FileInfo],
            ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
                Box::pin(async { Ok(Par2Result::default()) })
            }
            fn create_in_directory<'a>(
                &'a self,
                _: &'a CancellationToken,
                _: &'a [FileInfo],
                _: &'a str,
            ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
                Box::pin(async { Ok(Par2Result::default()) })
            }
            fn create_set<'a>(
                &'a self,
                _: &'a CancellationToken,
                _: &'a [FileInfo],
                _: &'a str,
                _: &'a str,
                _: &'a str,
            ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
                Box::pin(async { Ok(Par2Result::default()) })
            }
        }
        let inner: Arc<dyn Par2Executor> = Arc::new(Nop);
        assert!(Arc::ptr_eq(
            &inner,
            &ScheduledExecutor::wrap(inner.clone(), None)
        ));
        assert!(!Arc::ptr_eq(
            &inner,
            &ScheduledExecutor::wrap(inner.clone(), Some(Arc::new(Scheduler::new(2))))
        ));
    }

    #[tokio::test]
    async fn the_wrapper_gates_every_operation() {
        let s = Arc::new(Scheduler::new(1));
        struct Slow(Arc<AtomicUsize>, Arc<AtomicUsize>);
        impl Par2Executor for Slow {
            fn create<'a>(
                &'a self,
                _: &'a CancellationToken,
                _: &'a [FileInfo],
            ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
                Box::pin(async move {
                    let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
                    self.1.fetch_max(n, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    self.0.fetch_sub(1, Ordering::SeqCst);
                    Ok(Par2Result::default())
                })
            }
            fn create_in_directory<'a>(
                &'a self,
                c: &'a CancellationToken,
                f: &'a [FileInfo],
                _: &'a str,
            ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
                self.create(c, f)
            }
            fn create_set<'a>(
                &'a self,
                c: &'a CancellationToken,
                f: &'a [FileInfo],
                _: &'a str,
                _: &'a str,
                _: &'a str,
            ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
                self.create(c, f)
            }
        }
        let (cur, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let ex = ScheduledExecutor::wrap(Arc::new(Slow(cur, peak.clone())), Some(s));
        let c = CancellationToken::new();
        let (a, b, d) = tokio::join!(
            ex.create(&c, &[]),
            ex.create_in_directory(&c, &[], ""),
            ex.create_set(&c, &[], "", "x", "")
        );
        assert!(a.is_ok() && b.is_ok() && d.is_ok());
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }
}
