//! Provider pools and the multi-provider client.
//!
//! A [`ProviderPool`] owns one shared request queue and `connections` worker tasks. A worker sleeps
//! until work exists, connects (with backoff), serves requests with an `inflight` window until the
//! connection idles out, expires or breaks, and then goes back to sleep: connections are created in
//! proportion to demand and idle ones disappear. Because every worker pulls from the same queue,
//! load balances itself. A [`Client`] spreads requests round-robin across providers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::conn::{self, Answer, ConnCfg, End, Kind, Request, Scaler};
use crate::throttle::Throttle;
use crate::{Endpoint, NntpError};

/// Pool parameters for one provider (defaults match the Go config: 10 connections, inflight 10,
/// 300 s idle, 3600 s TTL).
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub endpoint: Endpoint,
    pub connections: usize,
    pub inflight: usize,
    pub idle_timeout: Duration,
    pub ttl: Duration,
    pub stall_timeout: Duration,
    /// Connection attempts per request before it fails, and the first backoff (doubles each try).
    pub connect_attempts: u32,
    pub connect_backoff: Duration,
}

impl ProviderConfig {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            connections: 10,
            inflight: 10,
            idle_timeout: Duration::from_secs(300),
            ttl: Duration::from_secs(3600),
            stall_timeout: Duration::from_secs(120),
            connect_attempts: 3,
            connect_backoff: Duration::from_millis(250),
        }
    }
}

/// Live counters for one provider.
#[derive(Default)]
pub struct ProviderStats {
    pub posted: AtomicU64,
    pub posted_bytes: AtomicU64,
    pub stat_present: AtomicU64,
    pub stat_missing: AtomicU64,
    pub errors: AtomicU64,
    pub connects: AtomicU64,
    pub active_connections: AtomicUsize,
}

/// A point-in-time copy of [`ProviderStats`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub name: String,
    pub posted: u64,
    pub posted_bytes: u64,
    pub stat_present: u64,
    pub stat_missing: u64,
    pub errors: u64,
    pub connects: u64,
    pub active_connections: usize,
}

pub struct ProviderPool {
    name: String,
    max_connections: usize,
    queue: async_channel::Sender<Request>,
    stats: Arc<ProviderStats>,
    scaler: Arc<Scaler>,
}

impl ProviderPool {
    /// Starts the pool; no connection is made until the first request.
    pub fn start(cfg: ProviderConfig, throttle: Option<Arc<Throttle>>) -> Arc<Self> {
        let (tx, rx) = async_channel::bounded::<Request>(cfg.connections.max(1) * 2);
        let stats = Arc::new(ProviderStats::default());
        let conn_cfg = ConnCfg {
            inflight: cfg.inflight.max(1),
            idle_timeout: cfg.idle_timeout,
            ttl: cfg.ttl,
            stall_timeout: cfg.stall_timeout,
        };
        let scaler = Arc::new(Scaler::default());
        for _ in 0..cfg.connections.max(1) {
            tokio::spawn(worker(
                cfg.clone(),
                conn_cfg.clone(),
                rx.clone(),
                stats.clone(),
                throttle.clone(),
                scaler.clone(),
            ));
        }
        Arc::new(Self {
            name: cfg.endpoint.key(),
            max_connections: cfg.connections.max(1),
            queue: tx,
            stats,
            scaler,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configured connection limit.
    pub fn max_connections(&self) -> usize {
        self.max_connections
    }

    async fn submit(&self, kind: Kind) -> Result<Answer, NntpError> {
        let (reply, rx) = oneshot::channel();
        self.queue
            .send(Request { kind, reply })
            .await
            .map_err(|_| NntpError::Closed)?;
        self.scaler.notify.notify_waiters();
        rx.await.unwrap_or(Err(NntpError::Closed))
    }

    /// Posts an article (headers, blank line, yEnc body); success is an explicit `240`.
    pub async fn post(&self, article: Vec<u8>) -> Result<(), NntpError> {
        let len = article.len() as u64;
        match self.submit(Kind::Post(article)).await {
            Ok(_) => {
                self.stats.posted.fetch_add(1, Ordering::Relaxed);
                self.stats.posted_bytes.fetch_add(len, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// `STAT <id>`: `Ok(true)` for `223`, `Ok(false)` for `430`, an error for anything else.
    pub async fn stat(&self, message_id: &str) -> Result<bool, NntpError> {
        match self.submit(Kind::Stat(message_id.to_string())).await {
            Ok(Answer::Present(p)) => {
                let c = if p {
                    &self.stats.stat_present
                } else {
                    &self.stats.stat_missing
                };
                c.fetch_add(1, Ordering::Relaxed);
                Ok(p)
            }
            Ok(Answer::Posted) => Err(NntpError::Protocol {
                code: 0,
                text: "bad answer".into(),
            }),
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    pub fn stats(&self) -> StatsSnapshot {
        let s = &self.stats;
        StatsSnapshot {
            name: self.name.clone(),
            posted: s.posted.load(Ordering::Relaxed),
            posted_bytes: s.posted_bytes.load(Ordering::Relaxed),
            stat_present: s.stat_present.load(Ordering::Relaxed),
            stat_missing: s.stat_missing.load(Ordering::Relaxed),
            errors: s.errors.load(Ordering::Relaxed),
            connects: s.connects.load(Ordering::Relaxed),
            active_connections: s.active_connections.load(Ordering::Relaxed),
        }
    }

    /// Stops accepting work; requests already in flight finish.
    pub fn close(&self) {
        self.queue.close();
        self.scaler.notify.notify_waiters();
    }
}

async fn worker(
    cfg: ProviderConfig,
    conn_cfg: ConnCfg,
    rx: async_channel::Receiver<Request>,
    stats: Arc<ProviderStats>,
    throttle: Option<Arc<Throttle>>,
    scaler: Arc<Scaler>,
) {
    loop {
        // A worker without a connection only wakes when every existing window is full (or there is
        // no connection yet), so light traffic reuses the connections it already has.
        let notified = scaler.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if rx.is_closed() && rx.is_empty() {
            return;
        }
        let first = if scaler.needs_more() {
            rx.try_recv().ok()
        } else {
            None
        };
        let Some(first) = first else {
            notified.await;
            continue;
        };
        // reserve the new connection's window before dialing so other workers do not pile on
        scaler.add_capacity(conn_cfg.inflight);
        scaler.add_used(1);
        let mut backoff = cfg.connect_backoff;
        let mut attempt = 0;
        let session = loop {
            attempt += 1;
            match conn::open(&cfg.endpoint).await {
                Ok(s) => break Ok(s),
                // bad credentials will not get better with retries
                Err(e @ NntpError::Auth { .. }) => break Err(e),
                Err(e) if attempt >= cfg.connect_attempts => break Err(e),
                Err(_) => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }
        };
        let session = match session {
            Ok(s) => s,
            Err(e) => {
                stats.errors.fetch_add(1, Ordering::Relaxed);
                scaler.remove_capacity(conn_cfg.inflight);
                scaler.remove_used(1);
                let _ = first.reply.send(Err(e));
                continue;
            }
        };
        stats.connects.fetch_add(1, Ordering::Relaxed);
        stats.active_connections.fetch_add(1, Ordering::Relaxed);
        let end = conn::run(
            session,
            &rx,
            &conn_cfg,
            Some(first),
            throttle.clone(),
            &scaler,
        )
        .await;
        stats.active_connections.fetch_sub(1, Ordering::Relaxed);
        scaler.remove_capacity(conn_cfg.inflight);
        // requests queued while this connection was ending need a worker: wake the others
        scaler.notify.notify_waiters();
        if end == End::QueueClosed {
            return;
        }
    }
}

/// Spreads work round-robin across providers.
pub struct Client {
    providers: Vec<Arc<ProviderPool>>,
    next: AtomicUsize,
}

impl Client {
    pub fn new(providers: Vec<Arc<ProviderPool>>) -> Self {
        Self {
            providers,
            next: AtomicUsize::new(0),
        }
    }

    /// Builds a client over `configs` sharing one throttle (`bytes_per_sec` 0 is unlimited).
    pub fn from_configs(configs: Vec<ProviderConfig>, bytes_per_sec: u64) -> Self {
        let throttle = (bytes_per_sec > 0).then(|| Arc::new(Throttle::new(bytes_per_sec)));
        Self::new(
            configs
                .into_iter()
                .map(|c| ProviderPool::start(c, throttle.clone()))
                .collect(),
        )
    }

    fn pick(&self) -> Option<&Arc<ProviderPool>> {
        if self.providers.is_empty() {
            return None;
        }
        Some(&self.providers[self.next.fetch_add(1, Ordering::Relaxed) % self.providers.len()])
    }

    /// Posts via the next provider in rotation.
    pub async fn post(&self, article: Vec<u8>) -> Result<(), NntpError> {
        self.pick().ok_or(NntpError::Closed)?.post(article).await
    }

    /// Checks the article on providers in rotation order until one has it. `Ok(false)` only when
    /// every provider answered `430`; if any provider failed and none had it, the error is returned
    /// (never "missing"), so verification cannot trigger false reposts.
    pub async fn stat(&self, message_id: &str) -> Result<bool, NntpError> {
        let n = self.providers.len();
        if n == 0 {
            return Err(NntpError::Closed);
        }
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        let mut last_err = None;
        for i in 0..n {
            match self.providers[(start + i) % n].stat(message_id).await {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => last_err = Some(e),
            }
        }
        last_err.map_or(Ok(false), Err)
    }

    pub fn stats(&self) -> Vec<StatsSnapshot> {
        self.providers.iter().map(|p| p.stats()).collect()
    }

    /// Total configured connections across providers.
    pub fn max_connections(&self) -> usize {
        self.providers.iter().map(|p| p.max_connections()).sum()
    }

    pub fn close(&self) {
        self.providers.iter().for_each(|p| p.close());
    }
}
