//! Port of `postcheck_retry_worker.go`: deferred article verification for the legacy path. Articles
//! whose immediate post-check failed are stored in `pending_article_checks`; this worker rechecks them
//! with batched STATs and exponential backoff, then settles each completed item's verification status.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::config::PostCheck;
use crate::queue::{PendingArticleCheck, Queue};
use crate::verification::Stater;

const DEFAULT_STAT_BATCH: usize = 100;

fn dur(d: &crate::config::Duration, default: Duration) -> Duration {
    u64::try_from(d.nanos())
        .ok()
        .filter(|n| *n > 0)
        .map_or(default, Duration::from_nanos)
}

struct Inner {
    queue: Queue,
    stater: Arc<dyn Stater>,
    enabled: bool,
    check_interval: Duration,
    initial_delay: Duration,
    max_backoff: Duration,
    max_retries: i64,
    batch_size: usize,
    stat_batch_size: usize,
    on_status_changed: Option<Arc<dyn Fn() + Send + Sync>>,
    cancel: CancellationToken,
}

#[derive(Clone)]
pub struct PostCheckRetryWorker(Arc<Inner>);

impl PostCheckRetryWorker {
    /// Zero/negative settings fall back to the defaults (interval 2m, initial delay 30s, max backoff
    /// 5m, 5 retries, batch 10000, STAT batch 100). `on_status_changed` fires once per batch and when an
    /// item's verification status changes.
    pub fn new(
        parent: &CancellationToken,
        queue: Queue,
        stater: Arc<dyn Stater>,
        cfg: &PostCheck,
        on_status_changed: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let pos = |v: i64, d: usize| usize::try_from(v).ok().filter(|n| *n > 0).unwrap_or(d);
        Self(Arc::new(Inner {
            queue,
            stater,
            enabled: cfg.enabled == Some(true),
            check_interval: dur(&cfg.deferred_check_interval, Duration::from_secs(120)),
            initial_delay: dur(&cfg.deferred_check_delay, Duration::from_secs(30)),
            max_backoff: dur(&cfg.deferred_max_backoff, Duration::from_secs(300)),
            max_retries: if cfg.deferred_max_retries <= 0 {
                5
            } else {
                cfg.deferred_max_retries
            },
            batch_size: pos(cfg.deferred_batch_size, 10_000),
            stat_batch_size: pos(cfg.stat_batch_size, DEFAULT_STAT_BATCH),
            on_status_changed,
            cancel: parent.child_token(),
        }))
    }

    /// Starts the loop; `None` (nothing spawned) when post-check is disabled.
    pub fn start(&self) -> Option<tokio::task::JoinHandle<()>> {
        if !self.0.enabled {
            crate::logx::info(
                "Post check retry worker not started (post check disabled)",
                &[("component", "processor".into())],
            );
            return None;
        }
        let me = self.clone();
        Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(me.0.check_interval);
            tick.tick().await; // first run after one interval, like Go's ticker
            loop {
                tokio::select! {
                    () = me.0.cancel.cancelled() => return,
                    _ = tick.tick() => {
                        while me.process_retries().await {
                            if me.0.cancel.is_cancelled() {
                                return;
                            }
                        }
                    }
                }
            }
        }))
    }

    pub fn stop(&self) {
        self.0.cancel.cancel();
    }

    /// `initial_delay * 2^retry_count`, capped at `max_backoff` (saturating, never wrapping).
    pub fn calculate_backoff(&self, retry_count: i64) -> Duration {
        let shift = u32::try_from(retry_count.clamp(0, 31)).unwrap_or(31);
        self.0
            .initial_delay
            .saturating_mul(1u32 << shift)
            .min(self.0.max_backoff)
    }

    pub fn failure_reason(&self, retry_count: i64) -> String {
        if retry_count >= self.0.max_retries {
            format!("exceeded max retries of {}", self.0.max_retries)
        } else {
            "unknown reason".into()
        }
    }

    fn notify(&self) {
        if let Some(f) = &self.0.on_status_changed {
            f();
        }
    }

    /// Checks one batch of due articles. Returns `true` when a full batch was processed (more may be
    /// waiting).
    pub async fn process_retries(&self) -> bool {
        let q = self.0.queue.clone();
        let limit = self.0.batch_size as i64;
        let articles =
            match tokio::task::spawn_blocking(move || q.get_articles_for_check(limit)).await {
                Ok(Ok(a)) => a,
                Ok(Err(e)) => {
                    crate::logx::warn(
                        &format!("Failed to get articles for deferred check: {e}"),
                        &[("component", "processor".into())],
                    );
                    return false;
                }
                Err(_) => return false,
            };
        if articles.is_empty() {
            return false;
        }
        let mut completed: HashSet<String> = HashSet::new();
        // malformed group lists are failed before the STAT sweep
        let mut checkable: Vec<PendingArticleCheck> = Vec::with_capacity(articles.len());
        for a in &articles {
            completed.insert(a.completed_item_id.clone());
            if serde_json::from_str::<Vec<String>>(&a.groups).is_err() {
                crate::logx::warn(
                    &format!("Failed to parse groups JSON: article={}", a.id),
                    &[("component", "processor".into())],
                );
                if let Err(e) = self.0.queue.mark_article_check_failed(a.id) {
                    crate::logx::warn(
                        &format!("Failed to mark article as failed: {e}"),
                        &[("component", "processor".into())],
                    );
                }
                continue;
            }
            checkable.push(a.clone());
        }
        let ids: Vec<String> = checkable.iter().map(|a| a.message_id.clone()).collect();
        let mut missing: HashSet<String> = HashSet::new();
        for chunk in ids.chunks(self.0.stat_batch_size) {
            if self.0.cancel.is_cancelled() {
                return false;
            }
            match self.0.stater.stat_batch(chunk).await {
                Ok(m) => missing.extend(m),
                Err(e) => {
                    crate::logx::warn(
                        &format!("Deferred STAT sweep interrupted: {e}"),
                        &[("component", "processor".into())],
                    );
                    return false;
                }
            }
        }
        let me = self.clone();
        let full = articles.len() == self.0.batch_size;
        let done =
            tokio::task::spawn_blocking(move || me.apply(&checkable, &missing, &completed)).await;
        if done.is_err() {
            return false;
        }
        // once per batch so per-item progress advances in the UI before items reach a final status
        self.notify();
        full
    }

    /// Writes the sweep's outcome: verified, failed, or rescheduled per article, then item statuses.
    fn apply(
        &self,
        checkable: &[PendingArticleCheck],
        missing: &HashSet<String>,
        completed: &HashSet<String>,
    ) {
        let q = &self.0.queue;
        for a in checkable {
            if self.0.cancel.is_cancelled() {
                return;
            }
            if !missing.contains(&a.message_id) {
                if let Err(e) = q.mark_article_verified(a.id) {
                    crate::logx::warn(
                        &format!(
                            "Failed to mark article as verified: article={} error={e}",
                            a.id
                        ),
                        &[("component", "processor".into())],
                    );
                }
                continue;
            }
            let new_count = a.retry_count + 1;
            if new_count >= self.0.max_retries {
                if let Err(e) = q.mark_article_check_failed(a.id) {
                    crate::logx::warn(
                        &format!(
                            "Failed to mark article as failed: article={} error={e}",
                            a.id
                        ),
                        &[("component", "processor".into())],
                    );
                }
                crate::logx::error(
                    &format!(
                        "Article verification permanently failed: messageID={} retries={new_count} maxRetries={}",
                        a.message_id, self.0.max_retries
                    ),
                    &[("component", "processor".into())],
                );
                continue;
            }
            let next = Utc::now()
                + chrono::Duration::from_std(self.calculate_backoff(new_count))
                    .unwrap_or_else(|_| chrono::Duration::minutes(5));
            if let Err(e) = q.update_article_check_retry(a.id, new_count, next) {
                crate::logx::warn(
                    &format!(
                        "Failed to update article check retry: article={} error={e}",
                        a.id
                    ),
                    &[("component", "processor".into())],
                );
            }
        }
        for id in completed {
            self.update_completed_item_status(id);
        }
    }

    /// Settles an item once no checks are pending: `verification_failed` if any failed, else `verified`.
    fn update_completed_item_status(&self, id: &str) {
        let q = &self.0.queue;
        let (total, pending, failed) = match q.get_pending_check_count_for_item(id) {
            Ok(c) => c,
            Err(e) => {
                return crate::logx::warn(
                    &format!("Failed to get pending check counts: item={id} error={e}"),
                    &[("component", "processor".into())],
                );
            }
        };
        if pending > 0 {
            return;
        }
        let status = if failed > 0 {
            crate::logx::warn(
                &format!(
                    "Completed item verification failed: item={id} total={total} failed={failed}"
                ),
                &[("component", "processor".into())],
            );
            "verification_failed"
        } else {
            "verified"
        };
        match q.update_completed_item_verification_status(id, status) {
            Ok(()) => self.notify(),
            Err(e) => crate::logx::warn(
                &format!(
                    "Failed to update completed item verification status: item={id} error={e}"
                ),
                &[("component", "processor".into())],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Duration as D;
    use crate::db::Db;
    use crate::poster::BoxFut;
    use std::collections::HashSet;
    use std::sync::Mutex;

    struct Fake(Mutex<HashSet<String>>, Mutex<bool>);
    impl Stater for Fake {
        fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, String>> {
            Box::pin(async move { Ok(self.0.lock().unwrap().contains(id)) })
        }
        fn stat_batch<'a>(
            &'a self,
            ids: &'a [String],
        ) -> BoxFut<'a, Result<HashSet<String>, String>> {
            Box::pin(async move {
                if *self.1.lock().unwrap() {
                    return Err("pool closed".into());
                }
                let m = self.0.lock().unwrap();
                Ok(ids.iter().filter(|i| m.contains(*i)).cloned().collect())
            })
        }
    }

    struct Env {
        w: PostCheckRetryWorker,
        q: Queue,
        fake: Arc<Fake>,
        item: String,
        notes: Arc<Mutex<u32>>,
    }

    fn env(missing: &[&str], cfg: PostCheck) -> Env {
        let db = Db::in_memory().unwrap();
        crate::database::MigrationRunner::new(db.clone())
            .migrate_up()
            .unwrap();
        let q = Queue::new(db);
        q.add_file("/a", 1).unwrap();
        let r = q.receive_file().unwrap().unwrap();
        q.complete_file(&r.msg_id, "/a.nzb", &r.job).unwrap();
        q.update_completed_item_verification_status(&r.msg_id, "pending_verification")
            .unwrap();
        let fake = Arc::new(Fake(
            Mutex::new(missing.iter().map(|s| s.to_string()).collect()),
            Mutex::new(false),
        ));
        let notes = Arc::new(Mutex::new(0));
        let n = notes.clone();
        let w = PostCheckRetryWorker::new(
            &CancellationToken::new(),
            q.clone(),
            fake.clone(),
            &cfg,
            Some(Arc::new(move || *n.lock().unwrap() += 1)),
        );
        Env {
            w,
            q,
            fake,
            item: r.msg_id,
            notes,
        }
    }

    fn cfg() -> PostCheck {
        PostCheck {
            enabled: Some(true),
            deferred_check_delay: D::new("1s"),
            deferred_max_retries: 3,
            deferred_max_backoff: D::new("1m"),
            deferred_batch_size: 100,
            ..Default::default()
        }
    }

    fn add(e: &Env, ids: &[&str]) {
        let due = Utc::now() - chrono::Duration::minutes(1);
        let arts: Vec<_> = ids
            .iter()
            .map(|m| PendingArticleCheck {
                id: 0,
                completed_item_id: String::new(),
                message_id: m.to_string(),
                groups: "[\"alt.x\"]".into(),
                status: String::new(),
                retry_count: 0,
                next_retry_at: due,
                first_failure_at: due,
                last_checked_at: None,
                created_at: due,
            })
            .collect();
        e.q.add_pending_article_checks(&e.item, &arts).unwrap();
    }

    fn status(e: &Env) -> String {
        e.q.db()
            .lock()
            .query_row(
                "SELECT verification_status FROM completed_items WHERE id=?1",
                [&e.item],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn defaults_and_backoff() {
        let e = env(&[], PostCheck::default());
        assert_eq!(
            (
                e.w.0.check_interval,
                e.w.0.initial_delay,
                e.w.0.max_backoff,
                e.w.0.max_retries,
                e.w.0.batch_size,
                e.w.0.stat_batch_size
            ),
            (
                Duration::from_secs(120),
                Duration::from_secs(30),
                Duration::from_secs(300),
                5,
                10_000,
                100
            )
        );
        let secs = |n| e.w.calculate_backoff(n).as_secs();
        assert_eq!((secs(0), secs(1), secs(2), secs(3)), (30, 60, 120, 240));
        assert_eq!((secs(4), secs(500)), (300, 300));
        assert_eq!(e.w.failure_reason(5), "exceeded max retries of 5");
        assert_eq!(e.w.failure_reason(1), "unknown reason");
    }

    #[tokio::test]
    async fn present_articles_verify_and_settle_the_item() {
        let e = env(&[], cfg());
        add(&e, &["<1>", "<2>"]);
        assert!(!e.w.process_retries().await);
        assert_eq!(status(&e), "verified");
        assert!(*e.notes.lock().unwrap() >= 1);
    }

    #[tokio::test]
    async fn missing_articles_are_rescheduled_then_failed_after_max_retries() {
        let e = env(&["<1>"], cfg());
        add(&e, &["<1>", "<2>"]);
        e.w.process_retries().await;
        assert_eq!(
            e.q.get_pending_check_count_for_item(&e.item).unwrap(),
            (2, 1, 0),
            "<2> verified, <1> rescheduled"
        );
        assert_eq!(status(&e), "pending_verification");
        for _ in 0..2 {
            e.q.db().lock().execute("UPDATE pending_article_checks SET next_retry_at='2000-01-01T00:00:00.000Z' WHERE status='pending'", []).unwrap();
            e.w.process_retries().await;
        }
        assert_eq!(
            e.q.get_pending_check_count_for_item(&e.item).unwrap(),
            (2, 0, 1)
        );
        assert_eq!(status(&e), "verification_failed");
    }

    #[tokio::test]
    async fn malformed_groups_fail_the_row_without_a_stat() {
        let e = env(&[], cfg());
        add(&e, &["<1>"]);
        e.q.db()
            .lock()
            .execute("UPDATE pending_article_checks SET groups='not json'", [])
            .unwrap();
        e.w.process_retries().await;
        assert_eq!(
            e.q.get_pending_check_count_for_item(&e.item).unwrap(),
            (1, 0, 1)
        );
        assert_eq!(status(&e), "verification_failed");
    }

    #[tokio::test]
    async fn an_interrupted_sweep_changes_nothing_and_a_full_batch_asks_for_more() {
        let e = env(
            &[],
            PostCheck {
                deferred_batch_size: 2,
                ..cfg()
            },
        );
        add(&e, &["<1>", "<2>", "<3>"]);
        *e.fake.1.lock().unwrap() = true;
        assert!(!e.w.process_retries().await);
        assert_eq!(
            e.q.get_pending_check_count_for_item(&e.item).unwrap(),
            (3, 3, 0)
        );
        *e.fake.1.lock().unwrap() = false;
        assert!(
            e.w.process_retries().await,
            "a full batch means more may be pending"
        );
        assert!(!e.w.process_retries().await);
        assert_eq!(status(&e), "verified");
    }

    #[tokio::test]
    async fn disabled_worker_does_not_start() {
        let e = env(
            &[],
            PostCheck {
                enabled: None,
                ..cfg()
            },
        );
        assert!(e.w.start().is_none());
        let e2 = env(&[], cfg());
        let h = e2.w.start().unwrap();
        e2.w.stop();
        h.await.unwrap();
    }
}
