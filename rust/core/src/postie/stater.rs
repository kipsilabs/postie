//! STAT checks for verification over a pool client, pipelined up to the pool's real capacity.
//!
//! One [`PoolStater`] is shared by every verification path, so its limits are process-wide: the
//! sweep of several files, the early checker and the failure rechecks all draw from the same
//! in-flight budget instead of each multiplying it.

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::Semaphore;

use crate::config::PostCheck;
use crate::poster::{ArticleClient, BoxFut};
use crate::verification::Stater;

/// Reports whether uploads are using the pool (verification then yields its connections).
pub type BusyFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// STATs in flight while uploads run on a pool that verification shares with them: STATs queue
/// behind article bodies on those connections, so more would only steal upload slots (issue #168).
const SHARED_POOL_BUSY_STATS: usize = 2;
/// In-flight STATs when the pool cannot report its capacity.
const FALLBACK_STATS: usize = 16;

/// In-flight STAT limits: `wide` always applies; `narrow` replaces it while the busy predicate holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatLimits {
    pub wide: usize,
    pub narrow: usize,
}

/// Limits from `post_check.max_concurrent_checks` (0 = auto) and the pool's pipelined capacity
/// (connections × inflight, 0 = unknown). Auto uses the full capacity; on a shared pool, while
/// uploads are busy, it drops to `reserved` (the pipelined capacity of the connections set aside
/// for checks), or to [`SHARED_POOL_BUSY_STATS`] when none are. An explicit value is used as-is
/// (capped at capacity) in both states.
pub fn stat_limits(
    configured: i64,
    shared_pool: bool,
    capacity: usize,
    reserved: usize,
) -> StatLimits {
    let cap = |n: usize| if capacity > 0 { n.min(capacity) } else { n }.max(1);
    if let Some(c) = usize::try_from(configured).ok().filter(|c| *c > 0) {
        let n = cap(c);
        return StatLimits { wide: n, narrow: n };
    }
    let wide = if capacity > 0 {
        capacity
    } else {
        FALLBACK_STATS
    };
    let narrow = if shared_pool {
        let busy = if reserved > 0 {
            reserved
        } else {
            SHARED_POOL_BUSY_STATS
        };
        busy.min(wide)
    } else {
        wide
    };
    StatLimits { wide, narrow }
}

/// Pools at least this large set aside a second check connection in auto mode.
const SECOND_CHECK_CONNECTION_AT: usize = 10;

/// Upload connections set aside for checking during upload when verification shares the upload
/// pool, always leaving one for uploads; none when that checking is off. Auto (0) takes 1, or 2
/// from [`SECOND_CHECK_CONNECTION_AT`] connections: pipelined STATs need little of a connection,
/// and every connection set aside is an upload slot lost.
pub fn reserved_check_connections(pc: &PostCheck, connections: usize) -> usize {
    if pc.check_during_upload == Some(false) {
        return 0;
    }
    let auto = if connections >= SECOND_CHECK_CONNECTION_AT {
        2
    } else {
        1
    };
    let want = usize::try_from(pc.check_connections)
        .ok()
        .filter(|n| *n > 0)
        .unwrap_or(auto);
    want.min(connections.saturating_sub(1))
}

/// Adapts a pool client to the verification service's `Stater`: a present article is `Ok(false)`
/// (not missing), a confirmed absence `Ok(true)`, and an error leaves presence unknown.
pub struct PoolStater {
    client: Arc<dyn ArticleClient>,
    wide: Semaphore,
    narrow: Semaphore,
    limits: StatLimits,
    busy: Option<BusyFn>,
}

impl PoolStater {
    /// `busy` switches to `limits.narrow` while it returns true (pass it only for a shared pool).
    pub fn new(client: Arc<dyn ArticleClient>, limits: StatLimits, busy: Option<BusyFn>) -> Self {
        let wide = limits.wide.max(1);
        let limits = StatLimits {
            wide,
            narrow: limits.narrow.clamp(1, wide),
        };
        Self {
            client,
            wide: Semaphore::new(limits.wide),
            narrow: Semaphore::new(limits.narrow),
            limits,
            busy,
        }
    }

    /// One STAT under the shared limits. The narrow permit is taken before the wide one so STATs
    /// waiting out a busy spell do not hold wide permits.
    async fn check(&self, id: &str) -> Result<bool, String> {
        let narrowed =
            self.limits.narrow < self.limits.wide && self.busy.as_ref().is_some_and(|b| b());
        // the semaphores are never closed, so acquire cannot fail
        let _narrow = if narrowed {
            self.narrow.acquire().await.ok()
        } else {
            None
        };
        let _wide = self.wide.acquire().await.ok();
        self.client.stat(id).await.map_err(|e| e.to_string())
    }
}

impl Stater for PoolStater {
    fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, String>> {
        Box::pin(async move { self.check(id).await.map(|present| !present) })
    }

    /// Ids not confirmed present; a per-id error counts as missing (the per-failure `stat` re-validates
    /// before any repost budget is spent).
    fn stat_batch<'a>(&'a self, ids: &'a [String]) -> BoxFut<'a, Result<HashSet<String>, String>> {
        Box::pin(async move {
            // a hand-rolled window: `buffer_unordered` over borrowed ids trips the `Send` + HRTB
            // inference of the boxed future
            let mut ids = ids.iter();
            let mut inflight = FuturesUnordered::new();
            let mut missing = HashSet::new();
            loop {
                while inflight.len() < self.limits.wide {
                    let Some(id) = ids.next() else { break };
                    inflight.push(async move { (id, self.check(id).await) });
                }
                let Some((id, r)) = inflight.next().await else {
                    break;
                };
                if !matches!(r, Ok(true)) {
                    missing.insert(id.clone());
                }
            }
            Ok(missing)
        })
    }

    fn window(&self) -> usize {
        self.limits.wide
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use postie_nntp::NntpError;

    #[test]
    fn auto_limits_use_the_pipelined_capacity_and_narrow_only_on_a_shared_pool() {
        assert_eq!(
            stat_limits(0, false, 100, 0),
            StatLimits {
                wide: 100,
                narrow: 100
            }
        );
        assert_eq!(
            stat_limits(0, true, 100, 0),
            StatLimits {
                wide: 100,
                narrow: 2
            }
        );
        assert_eq!(
            stat_limits(0, true, 1, 0),
            StatLimits { wide: 1, narrow: 1 }
        );
        assert_eq!(
            stat_limits(0, false, 0, 0),
            StatLimits {
                wide: 16,
                narrow: 16
            }
        );
    }

    #[test]
    fn reserved_check_connections_widen_the_busy_limit_on_a_shared_pool() {
        assert_eq!(
            stat_limits(0, true, 100, 20),
            StatLimits {
                wide: 100,
                narrow: 20
            }
        );
        assert_eq!(
            stat_limits(0, false, 100, 20),
            StatLimits {
                wide: 100,
                narrow: 100
            },
            "a dedicated pool never narrows"
        );
    }

    #[test]
    fn check_connections_leave_at_least_one_for_uploads() {
        let pc = |during: Option<bool>, n: i64| PostCheck {
            check_during_upload: during,
            check_connections: n,
            ..Default::default()
        };
        assert_eq!(reserved_check_connections(&pc(Some(true), 3), 10), 3);
        assert_eq!(
            reserved_check_connections(&pc(None, 0), 10),
            2,
            "auto, large pool"
        );
        assert_eq!(
            reserved_check_connections(&pc(None, 0), 4),
            1,
            "auto, small pool"
        );
        assert_eq!(
            reserved_check_connections(&pc(None, 0), 1),
            0,
            "uploads keep one"
        );
        assert_eq!(reserved_check_connections(&pc(Some(true), 8), 4), 3);
        assert_eq!(reserved_check_connections(&pc(Some(true), 2), 1), 0);
        assert_eq!(reserved_check_connections(&pc(Some(false), 3), 10), 0);
    }

    #[test]
    fn an_explicit_limit_is_capped_at_capacity_and_applies_when_busy_too() {
        assert_eq!(
            stat_limits(5, true, 100, 0),
            StatLimits { wide: 5, narrow: 5 }
        );
        assert_eq!(
            stat_limits(500, false, 100, 0),
            StatLimits {
                wide: 100,
                narrow: 100
            }
        );
        assert_eq!(
            stat_limits(7, false, 0, 0),
            StatLimits { wide: 7, narrow: 7 }
        );
    }

    /// Counts concurrent STATs; ids starting with `x` are missing.
    #[derive(Default)]
    struct Probe {
        now: AtomicUsize,
        peak: AtomicUsize,
    }
    impl ArticleClient for Probe {
        fn post(&self, _a: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
            Box::pin(async { Ok(()) })
        }
        fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
            Box::pin(async move {
                let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                self.now.fetch_sub(1, Ordering::SeqCst);
                Ok(!id.starts_with('x'))
            })
        }
        fn max_connections(&self) -> usize {
            1
        }
    }

    fn ids(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                if i % 10 == 0 {
                    format!("x{i}")
                } else {
                    format!("m{i}")
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn a_batch_keeps_the_wide_window_full_and_reports_the_misses() {
        let probe = Arc::new(Probe::default());
        let s = PoolStater::new(probe.clone(), StatLimits { wide: 8, narrow: 8 }, None);
        let missing = s.stat_batch(&ids(100)).await.unwrap();
        assert_eq!(missing.len(), 10);
        assert!(missing.iter().all(|id| id.starts_with('x')));
        assert_eq!(probe.peak.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn the_narrow_limit_applies_only_while_busy() {
        let probe = Arc::new(Probe::default());
        let busy = Arc::new(AtomicBool::new(true));
        let b = busy.clone();
        let s = PoolStater::new(
            probe.clone(),
            StatLimits { wide: 8, narrow: 2 },
            Some(Arc::new(move || b.load(Ordering::SeqCst))),
        );
        s.stat_batch(&ids(40)).await.unwrap();
        assert_eq!(probe.peak.load(Ordering::SeqCst), 2);
        busy.store(false, Ordering::SeqCst);
        s.stat_batch(&ids(40)).await.unwrap();
        assert_eq!(probe.peak.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn the_limit_is_shared_by_concurrent_callers() {
        let probe = Arc::new(Probe::default());
        let s = Arc::new(PoolStater::new(
            probe.clone(),
            StatLimits { wide: 4, narrow: 4 },
            None,
        ));
        let (a, b) = (ids(40), ids(40));
        let _ = tokio::join!(s.stat_batch(&a), s.stat_batch(&b));
        assert_eq!(probe.peak.load(Ordering::SeqCst), 4);
    }
}
