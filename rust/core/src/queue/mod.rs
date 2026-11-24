//! Port of `internal/queue`: the durable file-job queue (goqite + in-progress / completed / errored
//! tables), its listing API, removal, retry, script-retry bookkeeping and the legacy deferred article
//! checks. All SQL is the Go original's, against the same schema.
//!
//! Timestamps are written in UTC. (Go formatted `time.Now()` with a literal `Z` in the local zone in a
//! few legacy paths, which skewed them on non-UTC hosts; those are normalised here.)

mod legacy;
mod list;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, TimeZone, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub use legacy::PendingArticleCheck;
pub use list::{PaginatedResult, PaginationParams, QueueItem};

use crate::db::Db;
use crate::goqite;
use crate::manifest;
use crate::transferstore::{fmt_time, parse_time};

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_COMPLETE: &str = "complete";
pub const STATUS_ERROR: &str = "error";

const QUEUE_NAME: &str = "file_jobs";

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
}

pub type Result<T> = std::result::Result<T, QueueError>;

fn msg<T>(s: impl Into<String>) -> Result<T> {
    Err(QueueError::Msg(s.into()))
}

/// `2006-01-02T15:04:05.000Z` in UTC.
pub(crate) fn ms(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Go's zero `time.Time`.
pub(crate) fn zero_time() -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(1, 1, 1)
            .unwrap_or_default()
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default(),
    )
}

/// Parse that yields the zero time on failure, like `t, _ := time.Parse(...)`.
pub(crate) fn lenient_time(s: &str) -> DateTime<Utc> {
    parse_time(s).unwrap_or_else(|_| zero_time())
}

pub(crate) fn file_name_of(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or(path, |i| &path[i + 1..])
}

mod rfc {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        t: &DateTime<Utc>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&fmt_time(*t))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<DateTime<Utc>, D::Error> {
        let s = String::deserialize(d)?;
        parse_time(&s).map_err(serde::de::Error::custom)
    }
}

fn default_time() -> DateTime<Utc> {
    zero_time()
}

/// The persisted body of a queued job (same JSON as Go's `FileJob`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileJob {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub priority: i64,
    #[serde(rename = "createdAt", with = "rfc", default = "default_time")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "retryCount", default)]
    pub retry_count: i64,
    /// Stable id surviving retries and crash recovery (durable upload architecture).
    #[serde(
        rename = "transferId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub transfer_id: String,
    /// Root directory used to derive the relative path for NZB output.
    #[serde(
        rename = "inputFolder",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub input_folder: String,
    /// Per-job override of the global delete-original setting.
    #[serde(
        rename = "deleteOriginal",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub delete_original: Option<bool>,
}

/// Optional per-job fields when adding to the queue.
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    pub priority: i64,
    pub input_folder: String,
    pub delete_original: Option<bool>,
}

/// A job claimed by [`Queue::receive_file`].
#[derive(Debug, Clone)]
pub struct ReceivedJob {
    pub msg_id: String,
    pub job: FileJob,
}

#[derive(Clone)]
pub struct Queue {
    db: Db,
    q: goqite::Queue,
    /// Serialises check-then-insert in the duplicate-checking `add_*` methods (issue #184).
    add_mu: Arc<Mutex<()>>,
}

fn log_queued(path: &str, size: i64) {
    crate::logx::info(
        "Added to the upload queue",
        &[("path", path.into()), ("size", size.into())],
    );
}

impl Queue {
    /// Opens the queue and re-queues jobs that were in progress when the app last crashed.
    pub fn new(db: Db) -> Self {
        let q = Self::without_recovery(db);
        q.recover_in_progress_items();
        q
    }

    /// Opens the queue without re-queueing in-progress jobs (read-only inspection, tests).
    pub(crate) fn without_recovery(db: Db) -> Self {
        Self {
            q: goqite::Queue::new(db.clone(), QUEUE_NAME, 0, Duration::zero()),
            db,
            add_mu: Arc::new(Mutex::new(())),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    fn recover_in_progress_items(&self) {
        let rows: Vec<(String, String, i64, Vec<u8>)> = {
            let conn = self.db.lock();
            let Ok(mut stmt) =
                conn.prepare("SELECT id, path, priority, job_data FROM in_progress_items")
            else {
                return;
            };
            let Ok(it) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            else {
                return;
            };
            it.filter_map(std::result::Result::ok).collect()
        };
        let mut recovered = 0;
        for (id, path, priority, body) in rows {
            if self.q.send(&body, Duration::zero(), priority).is_err() {
                crate::logx::warn(
                    &format!("Failed to re-queue in-progress item: id={id} path={path}"),
                    &[("component", "queue".into())],
                );
                continue;
            }
            let _ = self
                .db
                .lock()
                .execute("DELETE FROM in_progress_items WHERE id = ?1", params![id]);
            recovered += 1;
        }
        if recovered > 0 {
            crate::logx::info(
                &format!("Recovered items from previous crash: count={recovered}"),
                &[("component", "queue".into())],
            );
        }
    }

    fn new_job(path: &str, size: i64, opts: &AddOptions) -> FileJob {
        FileJob {
            path: path.to_string(),
            size,
            priority: opts.priority,
            created_at: Utc::now(),
            retry_count: 0,
            transfer_id: uuid::Uuid::new_v4().to_string(),
            input_folder: opts.input_folder.clone(),
            delete_original: opts.delete_original,
        }
    }

    fn send_job(&self, job: &FileJob) -> Result<()> {
        self.q
            .send(&serde_json::to_vec(job)?, Duration::zero(), job.priority)?;
        Ok(())
    }

    fn add(&self, path: &str, size: i64, opts: &AddOptions, dedupe: bool) -> Result<()> {
        let _guard = dedupe.then(|| self.add_mu.lock().unwrap_or_else(|p| p.into_inner()));
        if dedupe
            && self
                .is_path_in_queue(path)
                .map_err(|e| QueueError::Msg(format!("failed to check if path exists: {e}")))?
        {
            return Ok(());
        }
        self.send_job(&Self::new_job(path, size, opts))?;
        log_queued(path, size);
        Ok(())
    }

    /// A file the user chose (picker, drop, import): queued unless the same path is still pending or
    /// uploading. Finished or failed runs keep their history and the path uploads again. Returns
    /// whether it was queued.
    pub fn add_user_file(&self, path: &str, size: i64, opts: AddOptions) -> Result<bool> {
        let _guard = self.add_mu.lock().unwrap_or_else(|p| p.into_inner());
        if self
            .is_path_active(path)
            .map_err(|e| QueueError::Msg(format!("failed to check if path is active: {e}")))?
        {
            return Ok(false);
        }
        self.send_job(&Self::new_job(path, size, &opts))?;
        log_queued(path, size);
        Ok(true)
    }

    /// Whether `path` is waiting in the queue or being uploaded right now.
    pub fn is_path_active(&self, path: &str) -> Result<bool> {
        let conn = self.db.lock();
        for (what, sql) in [
            (
                "pending queue",
                "SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs' AND job_path = ?1",
            ),
            (
                "in-progress items",
                "SELECT COUNT(*) FROM in_progress_items WHERE path = ?1",
            ),
        ] {
            let n: i64 = conn
                .query_row(sql, params![path], |r| r.get(0))
                .map_err(|e| QueueError::Msg(format!("failed to check {what}: {e}")))?;
            if n > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Adds a file unless its path is already tracked anywhere in the queue.
    pub fn add_file(&self, path: &str, size: i64) -> Result<()> {
        self.add(path, size, &AddOptions::default(), true)
    }

    /// A user-initiated upload: the original is always preserved regardless of the watcher policy.
    pub fn add_manual_file(&self, path: &str, size: i64) -> Result<()> {
        self.add_file_with_options(
            path,
            size,
            AddOptions {
                delete_original: Some(false),
                ..Default::default()
            },
        )
    }

    pub fn add_file_without_duplicate_check(&self, path: &str, size: i64) -> Result<()> {
        self.add(path, size, &AddOptions::default(), false)
    }

    pub fn add_file_with_priority(&self, path: &str, size: i64, priority: i64) -> Result<()> {
        self.add(
            path,
            size,
            &AddOptions {
                priority,
                ..Default::default()
            },
            true,
        )
    }

    pub fn add_file_with_priority_without_duplicate_check(
        &self,
        path: &str,
        size: i64,
        priority: i64,
    ) -> Result<()> {
        self.add(
            path,
            size,
            &AddOptions {
                priority,
                ..Default::default()
            },
            false,
        )
    }

    pub fn add_file_with_options(&self, path: &str, size: i64, opts: AddOptions) -> Result<()> {
        self.add(path, size, &opts, true)
    }

    /// Sum of pending job sizes (gates pickup behind `queue.min_size_to_start`).
    pub fn pending_total_size(&self) -> Result<i64> {
        self.db
            .lock()
            .query_row(
                "SELECT COALESCE(SUM(job_size), 0) FROM goqite WHERE queue = 'file_jobs'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| QueueError::Msg(format!("failed to compute pending total size: {e}")))
    }

    /// The job [`Queue::receive_file`] would take next, left in the queue (`None` when empty or
    /// unreadable).
    pub fn peek_file(&self) -> Result<Option<ReceivedJob>> {
        let Some(m) = self.q.peek()? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&m.body)
            .ok()
            .map(|job| ReceivedJob { msg_id: m.id, job }))
    }

    /// Takes the next job and tracks it in `in_progress_items` until completed or errored.
    pub fn receive_file(&self) -> Result<Option<ReceivedJob>> {
        let Some(m) = self.q.receive()? else {
            return Ok(None);
        };
        let job: FileJob = match serde_json::from_slice(&m.body) {
            Ok(j) => j,
            Err(e) => {
                let _ = self.q.delete(&m.id);
                return msg(format!("failed to unmarshal job: {e}"));
            }
        };
        // tracked before the delete so a crash in between can recover it; failure is non-fatal
        if let Err(e) = self.db.lock().execute(
            "INSERT OR REPLACE INTO in_progress_items (id, path, size, priority, created_at, job_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![m.id, job.path, job.size, job.priority, ms(job.created_at), m.body],
        ) {
            crate::logx::warn(&format!("Failed to track in-progress item: id={} error={e}", m.id), &[("component", "queue".into())]);
        }
        if let Err(e) = self.q.delete(&m.id) {
            let _ = self
                .db
                .lock()
                .execute("DELETE FROM in_progress_items WHERE id = ?1", params![m.id]);
            return msg(format!("failed to remove job from queue: {e}"));
        }
        Ok(Some(ReceivedJob { msg_id: m.id, job }))
    }

    pub fn complete_file(&self, msg_id: &str, nzb_path: &str, job: &FileJob) -> Result<()> {
        self.complete(msg_id, nzb_path, job, "verified")
    }

    /// Records reuse with the completion insert, so a crash cannot label reused content as verified.
    pub fn complete_reused_file(&self, msg_id: &str, nzb_path: &str, job: &FileJob) -> Result<()> {
        self.complete(msg_id, nzb_path, job, "reused")
    }

    fn complete(&self, msg_id: &str, nzb_path: &str, job: &FileJob, status: &str) -> Result<()> {
        let data = serde_json::to_vec(job)?;
        let conn = self.db.lock();
        conn.execute(
            "INSERT INTO completed_items (id, path, size, priority, nzb_path, created_at, job_data, verification_status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![msg_id, job.path, job.size, job.priority, nzb_path, ms(job.created_at), data, status],
        )
        .map_err(|e| QueueError::Msg(format!("failed to insert completed item: {e}")))?;
        let _ = conn.execute(
            "DELETE FROM in_progress_items WHERE id = ?1",
            params![msg_id],
        );
        Ok(())
    }

    /// Removes a cancelled job from in-progress tracking.
    pub fn cancel_file(&self, msg_id: &str) -> Result<()> {
        self.clear_in_progress(msg_id)
    }

    /// Drops the in-progress row; call before re-queueing a retry under a new id.
    pub fn clear_in_progress(&self, msg_id: &str) -> Result<()> {
        self.db
            .lock()
            .execute(
                "DELETE FROM in_progress_items WHERE id = ?1",
                params![msg_id],
            )
            .map(|_| ())
            .map_err(|e| QueueError::Msg(format!("failed to clear in-progress item {msg_id}: {e}")))
    }

    pub fn extend_timeout(&self, msg_id: &str, d: Duration) -> Result<()> {
        Ok(self.q.extend(msg_id, d)?)
    }

    /// Re-adds a job (used when processing fails).
    pub fn readd_job(&self, job: &FileJob) -> Result<()> {
        self.send_job(job)
    }

    pub fn mark_as_error(&self, msg_id: &str, job: &FileJob, err_msg: &str) -> Result<()> {
        let data = serde_json::to_vec(job)?;
        let conn = self.db.lock();
        conn.execute(
            "INSERT INTO errored_items (id, path, size, priority, error_message, created_at, job_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![msg_id, job.path, job.size, job.priority, err_msg, ms(job.created_at), data],
        )
        .map_err(|e| QueueError::Msg(format!("failed to insert errored item: {e}")))?;
        let _ = conn.execute(
            "DELETE FROM in_progress_items WHERE id = ?1",
            params![msg_id],
        );
        Ok(())
    }

    /// Moves an errored job back to the main queue with its retry count reset.
    pub fn retry_errored_job(&self, id: &str) -> Result<()> {
        let data: Option<Vec<u8>> = self
            .db
            .lock()
            .query_row(
                "SELECT job_data FROM errored_items WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(data) = data else {
            return msg(format!("errored item not found: {id}"));
        };
        let mut job: FileJob = serde_json::from_slice(&data)
            .map_err(|e| QueueError::Msg(format!("failed to unmarshal job data: {e}")))?;
        job.retry_count = 0;
        self.readd_job(&job)
            .map_err(|e| QueueError::Msg(format!("failed to re-add job to queue: {e}")))?;
        self.db
            .lock()
            .execute("DELETE FROM errored_items WHERE id = ?1", params![id])
            .map_err(|e| QueueError::Msg(format!("failed to delete errored item: {e}")))?;
        Ok(())
    }

    fn update_pending_job(&self, id: &str, priority: i64, set_column: bool) -> Result<()> {
        let body: Option<Vec<u8>> = self
            .db
            .lock()
            .query_row(
                "SELECT body FROM goqite WHERE id = ?1 AND queue = 'file_jobs'",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return msg(format!("pending queue item not found: {id}"));
        };
        let mut job: FileJob = serde_json::from_slice(&body)
            .map_err(|e| QueueError::Msg(format!("failed to unmarshal job: {e}")))?;
        job.priority = priority;
        let new_body = serde_json::to_vec(&job)?;
        let sql = if set_column {
            "UPDATE goqite SET body = ?1, priority = ?3, updated = strftime('%Y-%m-%dT%H:%M:%fZ') WHERE id = ?2 AND queue = 'file_jobs'"
        } else {
            "UPDATE goqite SET body = ?1, updated = strftime('%Y-%m-%dT%H:%M:%fZ') WHERE id = ?2 AND queue = 'file_jobs'"
        };
        let conn = self.db.lock();
        if set_column {
            conn.execute(sql, params![new_body, id, priority])
        } else {
            conn.execute(sql, params![new_body, id])
        }
        .map_err(|e| QueueError::Msg(format!("failed to update queue item priority: {e}")))?;
        Ok(())
    }

    /// Updates only the priority stored in the job body.
    pub fn set_queue_item_priority(&self, id: &str, priority: i64) -> Result<()> {
        self.update_pending_job(id, priority, false)
    }

    /// Updates the body and goqite's native priority column (higher numbers run first).
    pub fn set_queue_item_priority_with_reorder(&self, id: &str, priority: i64) -> Result<()> {
        self.update_pending_job(id, priority, true)
    }

    /// True when the path is pending, in progress, completed or errored.
    pub fn is_path_in_queue(&self, path: &str) -> Result<bool> {
        let conn = self.db.lock();
        for (what, sql) in [
            (
                "pending queue",
                "SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs' AND job_path = ?1",
            ),
            (
                "completed items",
                "SELECT COUNT(*) FROM completed_items WHERE path = ?1",
            ),
            (
                "in-progress items",
                "SELECT COUNT(*) FROM in_progress_items WHERE path = ?1",
            ),
            (
                "errored items",
                "SELECT COUNT(*) FROM errored_items WHERE path = ?1",
            ),
        ] {
            let n: i64 = conn
                .query_row(sql, params![path], |r| r.get(0))
                .map_err(|e| QueueError::Msg(format!("failed to check {what}: {e}")))?;
            if n > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn get_completed_item_nzb_path(&self, id: &str) -> Result<String> {
        let p: Option<String> = self
            .db
            .lock()
            .query_row(
                "SELECT nzb_path FROM completed_items WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        p.map_or_else(|| msg(format!("completed item not found: {id}")), Ok)
    }

    /// Runs a `DELETE ... RETURNING <path column>` and returns the paths of the rows it deleted.
    fn delete_returning_paths(&self, sql: &str, id: Option<&str>) -> Result<Vec<String>> {
        let conn = self.db.lock();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![id], |r| r.get::<_, Option<String>>(0))?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// True when any live row (pending, running, errored or completed) still references `path`.
    fn path_in_use(&self, path: &str) -> bool {
        let raw = path.strip_prefix("FOLDER:").unwrap_or(path);
        let folder = format!("FOLDER:{raw}");
        let conn = self.db.lock();
        [
            "SELECT EXISTS(SELECT 1 FROM in_progress_items WHERE path IN (?1, ?2))",
            "SELECT EXISTS(SELECT 1 FROM goqite WHERE queue = 'file_jobs' AND job_path IN (?1, ?2))",
            "SELECT EXISTS(SELECT 1 FROM errored_items WHERE path IN (?1, ?2))",
            "SELECT EXISTS(SELECT 1 FROM completed_items WHERE path IN (?1, ?2))",
        ]
        .iter()
        // on a query error assume in use: never delete on doubt
        .any(|sql| {
            conn.query_row(sql, params![raw, folder], |r| r.get::<_, bool>(0))
                .unwrap_or(true)
        })
    }

    /// Deletes the web-upload temp copies of rows that were just removed, unless another live row
    /// (notably a job the processor has just started) still references the same path.
    fn discard_unreferenced(&self, paths: &[String]) {
        for p in paths {
            if self.path_in_use(p) {
                continue;
            }
            crate::uploadtemp::discard_copy(p);
        }
    }

    /// Removes an item from whichever table holds it (completed, errored, in progress, then pending).
    /// A removed errored/pending/completed item that was a web-upload temp copy has its copy deleted too.
    pub fn remove_from_queue(&self, id: &str) -> Result<()> {
        let exists = |sql: &str| -> Result<bool> {
            Ok(self.db.lock().query_row(sql, params![id], |r| r.get(0))?)
        };
        if exists("SELECT EXISTS(SELECT 1 FROM completed_items WHERE id = ?1)")? {
            return self.remove_completed_item(id);
        }
        if exists("SELECT EXISTS(SELECT 1 FROM errored_items WHERE id = ?1)")? {
            return self.remove_errored_item(id);
        }
        if exists("SELECT EXISTS(SELECT 1 FROM in_progress_items WHERE id = ?1)")? {
            self.db
                .lock()
                .execute("DELETE FROM in_progress_items WHERE id = ?1", params![id])
                .map_err(|e| QueueError::Msg(format!("failed to remove in-progress item: {e}")))?;
            return Ok(());
        }
        let paths = self.delete_returning_paths(
            "DELETE FROM goqite WHERE queue = 'file_jobs' AND id = ?1 RETURNING job_path",
            Some(id),
        )?;
        self.discard_unreferenced(&paths);
        Ok(())
    }

    /// Removes a completed item, its legacy checks, its durable transfers and its NZB file.
    pub fn remove_completed_item(&self, id: &str) -> Result<()> {
        let row: Option<(String, Option<String>)> = self
            .db
            .lock()
            .query_row(
                "SELECT nzb_path, path FROM completed_items WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((nzb, job_path)) = row else {
            return msg(format!("completed item not found: {id}"));
        };
        self.db
            .lock()
            .execute(
                "DELETE FROM pending_article_checks WHERE completed_item_id = ?1",
                params![id],
            )
            .map_err(|e| {
                QueueError::Msg(format!("failed to delete pending article checks: {e}"))
            })?;
        self.detach_transfers("completed_item_id = ?1", Some(id))?;
        self.db
            .lock()
            .execute("DELETE FROM completed_items WHERE id = ?1", params![id])
            .map_err(|e| QueueError::Msg(format!("failed to delete completed item: {e}")))?;
        remove_nzb(&nzb);
        // durable mode keeps the upload copy until verification, which this removal just cancelled
        self.discard_unreferenced(&job_path.into_iter().collect::<Vec<_>>());
        Ok(())
    }

    /// Removes the durable transfer rows (and their failures and manifests) selected by `filter`.
    /// The background verification must stop for them, and above all the post-verification cleaner
    /// must never delete originals for an upload whose NZB the user has just thrown away.
    fn detach_transfers(&self, filter: &str, arg: Option<&str>) -> Result<()> {
        let sql = format!(
            "SELECT DISTINCT transfer_id, manifest_path FROM transfer_files WHERE {filter}"
        );
        let rows: Vec<(String, String)> = {
            let conn = self.db.lock();
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| QueueError::Msg(format!("failed to list transfer files: {e}")))?;
            let map = |r: &rusqlite::Row<'_>| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?));
            let it = match arg {
                Some(a) => stmt.query_map(params![a], map)?,
                None => stmt.query_map([], map)?,
            };
            it.collect::<std::result::Result<_, _>>()
                .map_err(|e| QueueError::Msg(format!("failed to scan transfer file: {e}")))?
        };
        let mut transfers: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
        transfers.sort_unstable();
        transfers.dedup();
        for t in transfers {
            let conn = self.db.lock();
            conn.execute(
                "DELETE FROM verification_failures WHERE transfer_id = ?1",
                params![t],
            )
            .map_err(|e| {
                QueueError::Msg(format!(
                    "failed to delete verification failures for {t}: {e}"
                ))
            })?;
            conn.execute(
                "DELETE FROM transfer_files WHERE transfer_id = ?1",
                params![t],
            )
            .map_err(|e| {
                QueueError::Msg(format!("failed to delete transfer files for {t}: {e}"))
            })?;
        }
        for (transfer, path) in &rows {
            if path.is_empty() {
                continue;
            }
            if let Err(e) = manifest::remove(std::path::Path::new(path), transfer) {
                crate::logx::warn(
                    &format!(
                        "Failed to remove manifest of detached transfer: path={path} error={e}"
                    ),
                    &[("component", "queue".into())],
                );
            }
        }
        Ok(())
    }

    pub fn remove_errored_item(&self, id: &str) -> Result<()> {
        let paths = self
            .delete_returning_paths(
                "DELETE FROM errored_items WHERE id = ?1 RETURNING path",
                Some(id),
            )
            .map_err(|e| QueueError::Msg(format!("failed to delete errored item: {e}")))?;
        self.discard_unreferenced(&paths);
        Ok(())
    }

    /// Deletes every completed row; returns (nzb paths, job paths) of the rows actually deleted.
    fn clear_completed_rows(&self) -> Result<(Vec<String>, Vec<String>)> {
        self.db
            .lock()
            .execute("DELETE FROM pending_article_checks", [])?;
        self.detach_transfers("completed_item_id IS NOT NULL", None)?;
        let conn = self.db.lock();
        let mut stmt = conn.prepare("DELETE FROM completed_items RETURNING nzb_path, path")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((
            rows.iter().map(|(n, _)| n.clone()).collect(),
            rows.into_iter().filter_map(|(_, p)| p).collect(),
        ))
    }

    /// Removes all completed, errored and active items.
    pub fn clear_queue(&self) -> Result<()> {
        let (nzbs, mut copies) = self.clear_completed_rows()?;
        copies.extend(self.delete_returning_paths(
            "DELETE FROM errored_items WHERE (?1 IS NULL OR id = ?1) RETURNING path",
            None,
        )?);
        nzbs.iter().for_each(|p| remove_nzb(p));
        copies.extend(self.delete_returning_paths(
            "DELETE FROM goqite WHERE queue = 'file_jobs' AND (?1 IS NULL OR id = ?1) RETURNING job_path",
            None,
        )?);
        self.discard_unreferenced(&copies);
        Ok(())
    }

    /// Removes only completed items and their NZB files.
    pub fn clear_completed_items(&self) -> Result<()> {
        let (nzbs, copies) = self.clear_completed_rows()?;
        nzbs.iter().for_each(|p| remove_nzb(p));
        self.discard_unreferenced(&copies);
        Ok(())
    }

    /// Counts per state plus the durable verification buckets (keys sort alphabetically in JSON, as in Go).
    pub fn get_queue_stats(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let conn = self.db.lock();
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0));
        let pending = count("SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'")?;
        let running = count("SELECT COUNT(*) FROM in_progress_items").unwrap_or(0);
        let complete = count("SELECT COUNT(*) FROM completed_items").unwrap_or(0);
        let error = count("SELECT COUNT(*) FROM errored_items").unwrap_or(0);
        let pv = count("SELECT COUNT(*) FROM completed_items WHERE verification_status = 'pending_verification'").unwrap_or(0);
        let vf = count("SELECT COUNT(*) FROM completed_items WHERE verification_status = 'verification_failed'").unwrap_or(0);
        let mut m = serde_json::Map::new();
        m.insert("pending".into(), pending.into());
        m.insert("total".into(), (pending + running).into());
        m.insert("running".into(), running.into());
        m.insert("complete".into(), complete.into());
        m.insert("error".into(), error.into());
        m.insert("pendingVerification".into(), pv.into());
        m.insert("verificationFailed".into(), vf.into());
        m.insert(
            "total_including_completed".into(),
            (pending + running + complete + error).into(),
        );
        Ok(m)
    }

    /// Visibility info for one pending job.
    pub fn debug_queue_item(&self, id: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
        type DebugRow = (i64, String, String, String);
        let row: Option<DebugRow> = self
            .db
            .lock()
            .query_row("SELECT received, timeout, created, updated FROM goqite WHERE id = ?1 AND queue = 'file_jobs'", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        let Some((received, timeout, created, updated)) = row else {
            return msg(format!("job not found: {id}"));
        };
        let now = Utc::now();
        let parsed = parse_time(&timeout).ok();
        let before = parsed.is_some_and(|t| now < t);
        let mut m = serde_json::Map::new();
        m.insert("id".into(), id.into());
        m.insert("received".into(), received.into());
        m.insert("timeout".into(), timeout.into());
        m.insert("created".into(), created.into());
        m.insert("updated".into(), updated.into());
        m.insert("currentTime".into(), ms(now).into());
        m.insert(
            "timeoutParseError".into(),
            if parsed.is_some() {
                serde_json::Value::Null
            } else {
                "parsing time failed".into()
            },
        );
        m.insert("isBeforeTimeout".into(), before.into());
        m.insert("shouldBeRunning".into(), (before && received > 0).into());
        if let Some(t) = parsed {
            m.insert(
                "timeoutDiff".into(),
                format!("{}s", (t - now).num_seconds()).into(),
            );
        }
        Ok(m)
    }

    /// Records a script execution outcome; `first_failure_at` is only stored while the column is NULL.
    pub fn update_script_status(
        &self,
        id: &str,
        status: &str,
        retry_count: i64,
        last_error: &str,
        next_retry_at: Option<DateTime<Utc>>,
        first_failure_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        self.db
            .lock()
            .execute(
                "UPDATE completed_items SET script_status = ?1, script_retry_count = ?2, script_last_error = ?3, script_next_retry_at = ?4,
                 script_first_failure_at = COALESCE(script_first_failure_at, ?5) WHERE id = ?6",
                params![status, retry_count, (!last_error.is_empty()).then_some(last_error), next_retry_at.map(ms), first_failure_at.map(ms), id],
            )
            .map(|_| ())
            .map_err(|e| QueueError::Msg(format!("failed to update script status: {e}")))
    }

    pub fn mark_script_completed(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .execute("UPDATE completed_items SET script_status = 'completed', script_last_error = NULL, script_next_retry_at = NULL, script_first_failure_at = NULL WHERE id = ?1", params![id])
            .map(|_| ())
            .map_err(|e| QueueError::Msg(format!("failed to mark script as completed: {e}")))
    }

    pub fn mark_script_failed(&self, id: &str, last_error: &str) -> Result<()> {
        self.db
            .lock()
            .execute("UPDATE completed_items SET script_status = 'failed_permanent', script_last_error = ?1, script_next_retry_at = NULL WHERE id = ?2", params![last_error, id])
            .map(|_| ())
            .map_err(|e| QueueError::Msg(format!("failed to mark script as failed: {e}")))
    }

    /// Completed items whose script is waiting to be retried and due.
    pub fn get_items_for_script_retry(&self, limit: i64) -> Result<Vec<CompletedItem>> {
        let conn = self.db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, path, size, priority, nzb_path, created_at, completed_at, job_data, script_retry_count, script_first_failure_at
                 FROM completed_items WHERE script_status = 'pending_retry' AND (script_next_retry_at IS NULL OR script_next_retry_at <= ?1)
                 ORDER BY script_next_retry_at ASC LIMIT ?2",
            )
            .map_err(|e| QueueError::Msg(format!("failed to query items for script retry: {e}")))?;
        let rows = stmt.query_map(params![ms(Utc::now()), limit], |r| {
            Ok(CompletedItem {
                id: r.get(0)?,
                path: r.get(1)?,
                size: r.get(2)?,
                priority: r.get(3)?,
                nzb_path: r.get(4)?,
                created_at: r
                    .get::<_, Option<String>>(5)?
                    .map_or_else(zero_time, |s| lenient_time(&s)),
                completed_at: r
                    .get::<_, Option<String>>(6)?
                    .map_or_else(zero_time, |s| lenient_time(&s)),
                job_data: r.get(7)?,
                script_retry_count: r.get(8)?,
                script_first_failure_at: r
                    .get::<_, Option<String>>(9)?
                    .and_then(|s| parse_time(&s).ok()),
            })
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }
}

/// A completed item as needed by the script-retry loop.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedItem {
    pub id: String,
    pub path: String,
    pub size: i64,
    pub priority: i64,
    pub nzb_path: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub job_data: Vec<u8>,
    pub script_retry_count: i64,
    pub script_first_failure_at: Option<DateTime<Utc>>,
}

fn remove_nzb(path: &str) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        crate::logx::warn(
            &format!("Failed to delete NZB file: path={path} error={e}"),
            &[("component", "queue".into())],
        );
    }
}

#[cfg(test)]
mod tests;
