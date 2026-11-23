//! Port of `script_retry_worker.go`: retries failed post-upload script executions with exponential
//! backoff, bounded by a retry count and a total retry duration.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::PostUploadScriptConfig;
use crate::queue::{CompletedItem, Queue};

const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(60);
/// Items fetched per pass, so a backlog cannot overwhelm the system.
const BATCH: i64 = 10;

pub struct ScriptRetryWorker {
    queue: Queue,
    cfg: PostUploadScriptConfig,
    cancel: CancellationToken,
    interval: Duration,
}

fn nanos(d: &crate::config::Duration) -> Option<chrono::Duration> {
    let n = d.nanos();
    (n > 0).then(|| chrono::Duration::nanoseconds(n))
}

impl ScriptRetryWorker {
    pub fn new(parent: &CancellationToken, queue: Queue, cfg: PostUploadScriptConfig) -> Self {
        let interval = nanos(&cfg.retry_check_interval)
            .and_then(|d| d.to_std().ok())
            .unwrap_or(DEFAULT_CHECK_INTERVAL);
        Self {
            queue,
            cfg,
            cancel: parent.child_token(),
            interval,
        }
    }

    /// Starts the loop (no-op, returning `None`, when script execution is disabled).
    pub fn start(self: Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if !self.cfg.enabled {
            crate::logx::info(
                "Post-upload script retry worker not started (script execution disabled)",
                &[("component", "processor".into())],
            );
            return None;
        }
        Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(self.interval);
            tick.tick().await; // Go's ticker first fires after one interval
            loop {
                tokio::select! {
                    () = self.cancel.cancelled() => return,
                    _ = tick.tick() => self.process_retries().await,
                }
            }
        }))
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }

    /// One pass over the due items.
    pub async fn process_retries(&self) {
        let q = self.queue.clone();
        let items =
            match tokio::task::spawn_blocking(move || q.get_items_for_script_retry(BATCH)).await {
                Ok(Ok(items)) => items,
                Ok(Err(e)) => {
                    return crate::logx::warn(
                        &format!("Failed to get items for script retry: {e}"),
                        &[("component", "processor".into())],
                    );
                }
                Err(_) => return,
            };
        for item in items {
            if let Err(e) = self.execute_script(&item).await {
                crate::logx::warn(
                    &format!(
                        "Script retry failed: item={} nzb={} error={e}",
                        item.id, item.nzb_path
                    ),
                    &[("component", "processor".into())],
                );
            }
        }
    }

    async fn run_command(&self, nzb_path: &str) -> (bool, String) {
        let command = self.cfg.command.replace("{nzb_path}", nzb_path);
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", &command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &command]);
            c
        };
        if let Some(dir) = Path::new(nzb_path).parent() {
            cmd.current_dir(dir);
        }
        cmd.kill_on_drop(true);
        let run = cmd.output();
        // a non-positive timeout means no limit (Go's zero timeout expired immediately)
        let out = match nanos(&self.cfg.timeout).and_then(|d| d.to_std().ok()) {
            Some(t) => match tokio::time::timeout(t, run).await {
                Ok(r) => r,
                Err(_) => {
                    return (
                        false,
                        format!("signal: killed, output: timed out after {t:?}"),
                    );
                }
            },
            None => run.await,
        };
        match out {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                (
                    o.status.success(),
                    if o.status.success() {
                        text
                    } else {
                        format!("{}, output: {text}", o.status)
                    },
                )
            }
            Err(e) => (false, format!("{e}, output: ")),
        }
    }

    /// Runs the script for one item and records the outcome.
    pub async fn execute_script(&self, item: &CompletedItem) -> Result<(), String> {
        crate::logx::info(
            &format!(
                "Retrying post-upload script: item={} nzb={}",
                item.id, item.nzb_path
            ),
            &[("component", "processor".into())],
        );
        let (ok, detail) = self.run_command(&item.nzb_path).await;
        let q = self.queue.clone();
        let id = item.id.clone();
        if ok {
            tokio::task::spawn_blocking(move || q.mark_script_completed(&id))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        let error_msg = format!("script failed: {detail}");
        let new_count = item.script_retry_count + 1;
        let now = Utc::now();
        let first_failure = item.script_first_failure_at.unwrap_or(now);
        if !self.should_retry(first_failure, new_count) {
            let reason = self.failure_reason(first_failure, new_count);
            let (q2, id2, msg) = (self.queue.clone(), item.id.clone(), error_msg.clone());
            if let Ok(Err(e)) =
                tokio::task::spawn_blocking(move || q2.mark_script_failed(&id2, &msg)).await
            {
                crate::logx::error(
                    &format!(
                        "Failed to mark script as permanently failed: item={} error={e}",
                        item.id
                    ),
                    &[("component", "processor".into())],
                );
            }
            return Err(format!("script failed permanently ({reason}): {detail}"));
        }
        let backoff = self.calculate_backoff(new_count);
        let next = now + backoff;
        let (q2, id2) = (self.queue.clone(), item.id.clone());
        if let Ok(Err(e)) = tokio::task::spawn_blocking(move || {
            q2.update_script_status(
                &id2,
                "pending_retry",
                new_count,
                &error_msg,
                Some(next),
                Some(first_failure),
            )
        })
        .await
        {
            crate::logx::warn(
                &format!(
                    "Failed to update script status for retry: item={} error={e}",
                    item.id
                ),
                &[("component", "processor".into())],
            );
        }
        Err(format!(
            "script retry failed, will retry in {}s: {detail}",
            backoff.num_seconds()
        ))
    }

    /// `retry_delay * 2^count`, capped at `max_backoff` when one is set (saturating instead of wrapping).
    pub fn calculate_backoff(&self, retry_count: i64) -> chrono::Duration {
        let base = self.cfg.retry_delay.nanos().max(0);
        let shift = u32::try_from(retry_count.clamp(0, 62)).unwrap_or(62);
        let backoff = base.saturating_mul(1i64 << shift);
        let max = self.cfg.max_backoff.nanos();
        chrono::Duration::nanoseconds(if max > 0 && backoff > max {
            max
        } else {
            backoff
        })
    }

    /// False once the total retry duration or the retry count is exhausted (a limit of 0 is unlimited).
    pub fn should_retry(&self, first_failure: chrono::DateTime<Utc>, retry_count: i64) -> bool {
        if nanos(&self.cfg.max_retry_duration).is_some_and(|max| Utc::now() - first_failure > max) {
            return false;
        }
        !(self.cfg.max_retries > 0 && retry_count >= self.cfg.max_retries)
    }

    pub fn failure_reason(&self, first_failure: chrono::DateTime<Utc>, retry_count: i64) -> String {
        if let Some(max) = nanos(&self.cfg.max_retry_duration)
            && Utc::now() - first_failure > max
        {
            return format!(
                "exceeded max retry duration of {}",
                crate::config::duration::format_duration(self.cfg.max_retry_duration.nanos())
            );
        }
        if self.cfg.max_retries > 0 && retry_count >= self.cfg.max_retries {
            return format!("exceeded max retries of {}", self.cfg.max_retries);
        }
        "unknown reason".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Duration as D;
    use crate::db::Db;

    fn worker(cfg: PostUploadScriptConfig) -> (ScriptRetryWorker, Queue) {
        let db = Db::in_memory().unwrap();
        crate::database::MigrationRunner::new(db.clone())
            .migrate_up()
            .unwrap();
        let q = Queue::new(db);
        (
            ScriptRetryWorker::new(&CancellationToken::new(), q.clone(), cfg),
            q,
        )
    }

    fn cfg(command: &str) -> PostUploadScriptConfig {
        PostUploadScriptConfig {
            enabled: true,
            command: command.into(),
            timeout: D::new("5s"),
            max_retries: 3,
            retry_delay: D::new("1m"),
            max_backoff: D::new("10m"),
            max_retry_duration: D::new("1h"),
            retry_check_interval: D::new("1s"),
        }
    }

    fn completed(q: &Queue, dir: &Path) -> CompletedItem {
        q.add_file("/a", 1).unwrap();
        let r = q.receive_file().unwrap().unwrap();
        let nzb = dir.join("out.nzb");
        std::fs::write(&nzb, b"<nzb/>").unwrap();
        q.complete_file(&r.msg_id, nzb.to_str().unwrap(), &r.job)
            .unwrap();
        q.update_script_status(
            &r.msg_id,
            "pending_retry",
            0,
            "first",
            Some(Utc::now() - chrono::Duration::minutes(1)),
            None,
        )
        .unwrap();
        q.get_items_for_script_retry(5).unwrap().remove(0)
    }

    fn tmp() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sr-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn script_status(q: &Queue, id: &str) -> (String, i64) {
        q.db()
            .lock()
            .query_row(
                "SELECT script_status, script_retry_count FROM completed_items WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn backoff_doubles_is_capped_and_never_wraps() {
        let (w, _) = worker(cfg("true"));
        let secs = |n| w.calculate_backoff(n).num_seconds();
        assert_eq!((secs(0), secs(1), secs(2), secs(3)), (60, 120, 240, 480));
        assert_eq!((secs(4), secs(200)), (600, 600));
        let (uncapped, _) = worker(PostUploadScriptConfig {
            max_backoff: D::new(""),
            ..cfg("true")
        });
        assert!(
            uncapped.calculate_backoff(200).num_seconds() > 0,
            "saturates instead of overflowing"
        );
    }

    #[test]
    fn retry_limits_by_count_and_duration() {
        let (w, _) = worker(cfg("true"));
        let now = Utc::now();
        assert!(w.should_retry(now, 2) && !w.should_retry(now, 3));
        assert!(!w.should_retry(now - chrono::Duration::hours(2), 1));
        assert_eq!(w.failure_reason(now, 3), "exceeded max retries of 3");
        assert!(
            w.failure_reason(now - chrono::Duration::hours(2), 1)
                .starts_with("exceeded max retry duration of 1h")
        );
        let (unlimited, _) = worker(PostUploadScriptConfig {
            max_retries: 0,
            max_retry_duration: D::new(""),
            ..cfg("true")
        });
        assert!(unlimited.should_retry(now - chrono::Duration::days(99), 1000));
    }

    #[tokio::test]
    async fn a_successful_run_marks_the_script_completed_with_the_nzb_path_substituted() {
        let dir = tmp();
        let marker = dir.join("marker");
        let (w, q) = worker(cfg(&format!("echo {{nzb_path}} > {}", marker.display())));
        let item = completed(&q, &dir);
        w.execute_script(&item).await.unwrap();
        assert_eq!(script_status(&q, &item.id).0, "completed");
        assert_eq!(
            std::fs::read_to_string(marker).unwrap().trim(),
            item.nzb_path
        );
    }

    #[tokio::test]
    async fn a_failing_run_schedules_a_retry_then_fails_permanently() {
        let dir = tmp();
        let (w, q) = worker(PostUploadScriptConfig {
            max_retries: 2,
            ..cfg(if cfg!(windows) {
                "echo nope & exit 3"
            } else {
                "echo nope; exit 3"
            })
        });
        let item = completed(&q, &dir);
        assert!(
            w.execute_script(&item)
                .await
                .unwrap_err()
                .contains("will retry in")
        );
        assert_eq!(script_status(&q, &item.id), ("pending_retry".into(), 1));
        let again = {
            q.update_script_status(
                &item.id,
                "pending_retry",
                1,
                "x",
                Some(Utc::now() - chrono::Duration::seconds(1)),
                None,
            )
            .unwrap();
            q.get_items_for_script_retry(5).unwrap().remove(0)
        };
        let err = w.execute_script(&again).await.unwrap_err();
        assert!(
            err.contains("failed permanently (exceeded max retries of 2)") && err.contains("nope"),
            "{err}"
        );
        assert_eq!(script_status(&q, &item.id).0, "failed_permanent");
    }

    #[tokio::test]
    async fn a_script_over_its_timeout_is_killed_and_counts_as_a_failure() {
        let dir = tmp();
        let (w, q) = worker(PostUploadScriptConfig {
            timeout: D::new("100ms"),
            ..cfg("sleep 5")
        });
        let item = completed(&q, &dir);
        assert!(w.execute_script(&item).await.is_err());
        assert_eq!(script_status(&q, &item.id).0, "pending_retry");
    }

    #[tokio::test]
    async fn the_loop_processes_due_items_and_does_not_start_when_disabled() {
        let dir = tmp();
        let (w, q) = worker(cfg("true"));
        assert!(
            Arc::new(ScriptRetryWorker {
                cfg: PostUploadScriptConfig {
                    enabled: false,
                    ..cfg("true")
                },
                ..worker(cfg("true")).0
            })
            .start()
            .is_none()
        );
        let item = completed(&q, &dir);
        w.process_retries().await;
        assert_eq!(script_status(&q, &item.id).0, "completed");
    }
}
