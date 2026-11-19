//! Port of `internal/poster/engine.go`: the process-wide upload resource owner. It bounds how many
//! articles are posted concurrently (worker slots) and how much memory in-flight article buffers may
//! reserve (buffer budget), independent of how many queue jobs are active. One [`Engine`] is shared by
//! every job. Go passes a nil engine for standalone use; here the poster holds an `Option<Arc<Engine>>`.
//!
//! Reservations are RAII permits (released on drop) instead of Go's paired reserve/release calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::throttle::Throttle;
use crate::uploadmeter::UploadMeter;

/// Headers, yEnc framing slack and encoder working buffers beyond the raw + encoded body.
const ARTICLE_OVERHEAD_BYTES: i64 = 256 * 1024;
pub const MIN_AUTO_BUDGET_BYTES: i64 = 64 * 1024 * 1024;
pub const MAX_AUTO_BUDGET_BYTES: i64 = 512 * 1024 * 1024;

/// The resolved process-wide upload limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineBudget {
    /// Memory reserved for one in-flight article (raw body + estimated encoded size + overhead).
    pub per_article_bytes: i64,
    /// Total buffer memory the engine may reserve at once.
    pub budget_bytes: i64,
    /// Maximum number of articles posted concurrently.
    pub worker_count: i64,
}

/// yEnc adds roughly 2-3% plus line endings; 5% is a safe upper bound.
fn estimated_yenc_bytes(article_size: i64) -> i64 {
    article_size + article_size / 20
}

/// Resolves the budget from the article size, an explicit buffer limit (0 = auto) and the total
/// connection capacity (sum of `max_connections * effective inflight` across upload providers):
///
/// ```text
/// per_article  = article_size + estimated_yenc_size + 256 KiB
/// auto_budget  = clamp(per_article * conn_capacity, 64 MiB, 512 MiB)
/// worker_count = conn_capacity
/// ```
///
/// The worker count always equals the connection capacity so posting concurrency matches what the
/// pool can drive; the byte budget bounds only in-flight buffer memory (deriving workers from the
/// budget capped throughput at ~37 articles, issue #168). `budget >= per_article` and
/// `worker_count >= 1` always hold so callers can make progress.
pub fn compute_engine_budget(
    article_size: u64,
    buffer_limit: i64,
    conn_capacity: i64,
) -> EngineBudget {
    let article_size = if article_size == 0 {
        768 * 1024
    } else {
        article_size
    };
    let conn_capacity = conn_capacity.max(1);
    let raw = i64::try_from(article_size).unwrap_or(i64::MAX / 4);
    let per_article = raw + estimated_yenc_bytes(raw) + ARTICLE_OVERHEAD_BYTES;
    let mut budget = buffer_limit;
    if budget <= 0 {
        budget = (per_article * conn_capacity).clamp(MIN_AUTO_BUDGET_BYTES, MAX_AUTO_BUDGET_BYTES);
    }
    EngineBudget {
        per_article_bytes: per_article,
        budget_bytes: budget.max(per_article),
        worker_count: conn_capacity,
    }
}

/// Why a reservation or worker slot could not be acquired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("context canceled")]
pub struct Canceled;

/// Point-in-time snapshot of engine activity.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Metrics {
    pub active_workers: i64,
    pub queued_workers: i64,
    pub worker_count: i64,
    pub reserved_bytes: i64,
    pub budget_bytes: i64,
    pub per_article_byte: i64,
    pub upload_bytes: i64,
    pub upload_speed: f64,
    pub upload_avg_speed: f64,
}

/// A held share of the engine's buffer budget; released on drop.
pub struct BufferPermit {
    _permit: OwnedSemaphorePermit,
    reserved: Arc<AtomicI64>,
    bytes: i64,
}

impl Drop for BufferPermit {
    fn drop(&mut self) {
        self.reserved.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// A held worker slot; released on drop.
pub struct WorkerPermit {
    _permit: OwnedSemaphorePermit,
    active: Arc<AtomicI64>,
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Engine {
    budget: EngineBudget,
    workers: Arc<Semaphore>,
    memory: Arc<Semaphore>,
    active: Arc<AtomicI64>,
    queued: AtomicI64,
    reserved: Arc<AtomicI64>,
    throttle: std::sync::OnceLock<Option<Arc<Throttle>>>,
    meter: UploadMeter,
}

impl Engine {
    /// Sized for the article size, explicit buffer limit (0 = auto) and upload connection capacity.
    pub fn new(article_size: u64, buffer_limit: i64, conn_capacity: i64) -> Self {
        let b = compute_engine_budget(article_size, buffer_limit, conn_capacity);
        Self {
            budget: b,
            workers: Arc::new(Semaphore::new(usize::try_from(b.worker_count).unwrap_or(1))),
            memory: Arc::new(Semaphore::new(
                usize::try_from(b.budget_bytes).unwrap_or(usize::MAX >> 4),
            )),
            active: Arc::default(),
            queued: AtomicI64::new(0),
            reserved: Arc::default(),
            throttle: std::sync::OnceLock::new(),
            meter: UploadMeter::new(),
        }
    }

    pub fn budget(&self) -> EngineBudget {
        self.budget
    }

    pub fn per_article_bytes(&self) -> i64 {
        self.budget.per_article_bytes
    }

    pub fn upload_meter(&self) -> &UploadMeter {
        &self.meter
    }

    /// The engine-wide throttle (the first caller's rate wins), shared by every posting path so the
    /// configured rate bounds aggregate egress. `None` when `rate <= 0`.
    pub fn shared_throttle(&self, rate: i64) -> Option<Arc<Throttle>> {
        if rate <= 0 {
            return None;
        }
        self.throttle
            .get_or_init(|| Some(Arc::new(Throttle::new(rate))))
            .clone()
    }

    /// Waits until `n` bytes of buffer budget are free. A non-positive `n` reserves nothing.
    pub async fn reserve_buffer(
        &self,
        cancel: &CancellationToken,
        n: i64,
    ) -> Result<Option<BufferPermit>, Canceled> {
        if n <= 0 {
            return Ok(None);
        }
        let n32 = u32::try_from(n).unwrap_or(u32::MAX);
        let permit = tokio::select! {
            () = cancel.cancelled() => return Err(Canceled),
            p = self.memory.clone().acquire_many_owned(n32) => p.map_err(|_| Canceled)?,
        };
        self.reserved.fetch_add(n, Ordering::Relaxed);
        Ok(Some(BufferPermit {
            _permit: permit,
            reserved: self.reserved.clone(),
            bytes: n,
        }))
    }

    /// Waits for a free upload worker slot.
    pub async fn acquire_worker(
        &self,
        cancel: &CancellationToken,
    ) -> Result<WorkerPermit, Canceled> {
        self.queued.fetch_add(1, Ordering::Relaxed);
        let r = tokio::select! {
            () = cancel.cancelled() => Err(Canceled),
            p = self.workers.clone().acquire_owned() => p.map_err(|_| Canceled),
        };
        self.queued.fetch_sub(1, Ordering::Relaxed);
        let permit = r?;
        self.active.fetch_add(1, Ordering::Relaxed);
        Ok(WorkerPermit {
            _permit: permit,
            active: self.active.clone(),
        })
    }

    pub fn metrics(&self) -> Metrics {
        let up = self.meter.snapshot();
        Metrics {
            active_workers: self.active.load(Ordering::Relaxed),
            queued_workers: self.queued.load(Ordering::Relaxed),
            worker_count: self.budget.worker_count,
            reserved_bytes: self.reserved.load(Ordering::Relaxed),
            budget_bytes: self.budget.budget_bytes,
            per_article_byte: self.budget.per_article_bytes,
            upload_bytes: up.bytes_uploaded,
            upload_speed: up.speed,
            upload_avg_speed: up.avg_speed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    const ART: u64 = 768 * 1024;

    #[test]
    fn auto_sizing_is_clamped_and_workers_match_capacity() {
        let b = compute_engine_budget(ART, 0, 40);
        assert!(b.per_article_bytes > ART as i64);
        assert!((MIN_AUTO_BUDGET_BYTES..=MAX_AUTO_BUDGET_BYTES).contains(&b.budget_bytes));
        assert_eq!(b.worker_count, 40);
        assert!(compute_engine_budget(ART, 0, 1000).budget_bytes <= MAX_AUTO_BUDGET_BYTES);
    }

    #[test]
    fn explicit_limit_bounds_memory_not_workers() {
        let per = compute_engine_budget(ART, 0, 1).per_article_bytes;
        let b = compute_engine_budget(ART, per * 3, 100);
        assert_eq!((b.worker_count, b.budget_bytes), (100, per * 3));
    }

    /// Regression for issue #168: workers equal connection capacity and the auto budget fits them.
    #[test]
    fn workers_match_connection_capacity() {
        let b = compute_engine_budget(750_000, 0, 100);
        assert_eq!(b.worker_count, 100);
        assert!(
            b.budget_bytes >= b.per_article_bytes * 100 || b.budget_bytes == MAX_AUTO_BUDGET_BYTES
        );
    }

    #[test]
    fn tiny_limit_still_allows_one_article() {
        let b = compute_engine_budget(ART, 1024, 8);
        assert!(b.budget_bytes >= b.per_article_bytes && b.worker_count >= 1);
        assert_eq!(
            compute_engine_budget(0, 0, 0).worker_count,
            1,
            "zero article size and capacity fall back"
        );
    }

    #[tokio::test]
    async fn buffer_budget_is_bounded() {
        let per = compute_engine_budget(ART, 0, 1).per_article_bytes;
        let e = Arc::new(Engine::new(ART, per * 2, 100));
        let c = CancellationToken::new();
        let _a = e.reserve_buffer(&c, per).await.unwrap();
        let _b = e.reserve_buffer(&c, per).await.unwrap();
        assert_eq!(e.metrics().reserved_bytes, per * 2);
        let (e2, c2) = (e.clone(), c.clone());
        let third =
            tokio::spawn(async move { e2.reserve_buffer(&c2, per).await.map(|p| p.is_some()) });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !third.is_finished(),
            "third reservation must block when the budget is exhausted"
        );
        drop(_a);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), third)
                .await
                .unwrap()
                .unwrap(),
            Ok(true)
        );
    }

    #[tokio::test]
    async fn worker_slots_are_bounded() {
        let e = Arc::new(Engine::new(ART, 0, 2));
        let (cur, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut hs = Vec::new();
        for _ in 0..8 {
            let (e, cur, max) = (e.clone(), cur.clone(), max.clone());
            hs.push(tokio::spawn(async move {
                let _w = e.acquire_worker(&CancellationToken::new()).await.unwrap();
                let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                cur.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in hs {
            h.await.unwrap();
        }
        assert!(
            max.load(Ordering::SeqCst) <= 2,
            "max concurrent workers = {}",
            max.load(Ordering::SeqCst)
        );
        assert_eq!(e.metrics().active_workers, 0);
    }

    #[tokio::test]
    async fn cancelled_reservation_does_not_count() {
        let per = compute_engine_budget(ART, 0, 1).per_article_bytes;
        let e = Arc::new(Engine::new(ART, per, 100));
        let _held = e
            .reserve_buffer(&CancellationToken::new(), per)
            .await
            .unwrap();
        let c = CancellationToken::new();
        let (e2, c2) = (e.clone(), c.clone());
        let waiting = tokio::spawn(async move { e2.reserve_buffer(&c2, per).await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(20)).await;
        c.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), waiting)
                .await
                .unwrap()
                .unwrap(),
            Err(Canceled)
        );
        assert_eq!(e.metrics().reserved_bytes, per);
    }

    #[test]
    fn shared_throttle_is_created_once_and_zero_rate_is_none() {
        let e = Engine::new(ART, 0, 2);
        assert!(e.shared_throttle(0).is_none());
        let a = e.shared_throttle(1000).unwrap();
        let b = e.shared_throttle(999_999).unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "the first caller's rate wins and the bucket is shared"
        );
    }

    #[test]
    fn metrics_include_upload_totals() {
        let e = Engine::new(750_000, 0, 4);
        e.upload_meter().record(4096);
        assert_eq!(e.metrics().upload_bytes, 4096);
    }
}
