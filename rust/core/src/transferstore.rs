//! Port of `internal/transferstore`: durable persistence for the process-wide upload architecture
//! (issue 184): the `transfer_files` table (one row per source/PAR2 file pointing at an immutable
//! manifest) and `verification_failures` (only articles that failed a STAT check, with database
//! leases so verification survives crashes). Successful articles are never stored.
//!
//! Timestamps are stored as RFC 3339 text with nanoseconds and trailing zeros trimmed (Go's
//! `time.RFC3339Nano`), because they are compared as strings in SQL; reading also accepts SQLite's
//! `strftime` default format.

use chrono::{DateTime, Timelike, Utc};
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};

use crate::db::Db;

pub const STATE_PLANNED: &str = "planned";
pub const STATE_UPLOADING: &str = "uploading";
pub const STATE_UPLOADED: &str = "uploaded";
pub const STATE_VERIFYING: &str = "verifying";
pub const STATE_VERIFIED: &str = "verified";
pub const STATE_VERIFICATION_FAILED: &str = "verification_failed";

/// Cleanup policy marker: delete the source after verification. Empty retains it.
pub const CLEANUP_DELETE_ORIGINAL: &str = "delete_original";

/// Synthetic file id for failures migrated from `pending_article_checks` (STAT-only, never re-posted).
pub const LEGACY_FILE_ID: &str = "legacy";

pub const FAILURE_PENDING: &str = "pending";
pub const FAILURE_RESOLVED: &str = "resolved";
pub const FAILURE_FAILED: &str = "failed";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{ctx}: {source}")]
    Wrapped {
        ctx: &'static str,
        source: rusqlite::Error,
    },
    #[error("unparseable timestamp {0:?}")]
    Timestamp(String),
}

type Result<T> = std::result::Result<T, StoreError>;

fn wrap(ctx: &'static str) -> impl FnOnce(rusqlite::Error) -> StoreError {
    move |source| StoreError::Wrapped { ctx, source }
}

/// One row of `transfer_files`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferFile {
    pub transfer_id: String,
    pub file_id: String,
    pub completed_item_id: String,
    pub manifest_path: String,
    pub manifest_version: i64,
    pub source_path: String,
    pub file_role: String,
    pub article_count: i64,
    pub upload_state: String,
    pub verification_state: String,
    pub posted_at: Option<DateTime<Utc>>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub cleanup_policy: String,
    pub last_error: String,
    /// Failed attempts to start this file's first verification check, so the service can back off
    /// and eventually terminalize instead of retrying forever.
    pub check_attempts: i64,
}

/// One row of `verification_failures`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationFailure {
    pub id: i64,
    pub transfer_id: String,
    pub file_id: String,
    pub article_index: i64,
    pub message_id: String,
    pub groups: Vec<String>,
    pub repost_count: i64,
    pub deferred_count: i64,
    pub state: String,
    pub next_attempt_at: DateTime<Utc>,
    pub lease_owner: String,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error: String,
    pub last_checked_at: Option<DateTime<Utc>>,
}

impl Default for VerificationFailure {
    fn default() -> Self {
        Self {
            id: 0,
            transfer_id: String::new(),
            file_id: String::new(),
            article_index: 0,
            message_id: String::new(),
            groups: vec![],
            repost_count: 0,
            deferred_count: 0,
            state: String::new(),
            next_attempt_at: DateTime::<Utc>::MIN_UTC,
            lease_owner: String::new(),
            lease_expires_at: None,
            last_error: String::new(),
            last_checked_at: None,
        }
    }
}

/// Go's `time.RFC3339Nano` in UTC: fraction present only when non-zero, trailing zeros trimmed.
pub fn fmt_time(t: DateTime<Utc>) -> String {
    let mut s = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let nanos = t.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        s.push('.');
        s.push_str(format!("{nanos:09}").trim_end_matches('0'));
    }
    s.push('Z');
    s
}

fn fmt_ptr(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(fmt_time)
}

/// Parses a timestamp written by Go (RFC3339Nano) or by SQLite's strftime default.
pub fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| StoreError::Timestamp(s.to_string()))
}

fn parse_ptr(s: Option<String>) -> Result<Option<DateTime<Utc>>> {
    match s.as_deref() {
        None | Some("") => Ok(None),
        Some(v) => parse_time(v).map(Some),
    }
}

fn null_str(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

const TRANSFER_FILE_COLS: &str =
    "transfer_id, file_id, completed_item_id, manifest_path, manifest_version,
    source_path, file_role, article_count, upload_state, verification_state,
    posted_at, next_check_at, cleanup_policy, last_error, check_attempts";

fn scan_file(r: &Row<'_>) -> Result<TransferFile> {
    Ok(TransferFile {
        transfer_id: r.get(0)?,
        file_id: r.get(1)?,
        completed_item_id: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        manifest_path: r.get(3)?,
        manifest_version: r.get(4)?,
        source_path: r.get(5)?,
        file_role: r.get(6)?,
        article_count: r.get(7)?,
        upload_state: r.get(8)?,
        verification_state: r.get(9)?,
        posted_at: parse_ptr(r.get(10)?)?,
        next_check_at: parse_ptr(r.get(11)?)?,
        cleanup_policy: r.get(12)?,
        last_error: r.get(13)?,
        check_attempts: r.get(14)?,
    })
}

const FAILURE_COLS: &str = "id, transfer_id, file_id, article_index, message_id, groups,
    repost_count, deferred_count, state, next_attempt_at, lease_owner, lease_expires_at,
    last_error, last_checked_at";

fn scan_failure(r: &Row<'_>) -> Result<VerificationFailure> {
    let groups: String = r.get(5)?;
    Ok(VerificationFailure {
        id: r.get(0)?,
        transfer_id: r.get(1)?,
        file_id: r.get(2)?,
        article_index: r.get(3)?,
        message_id: r.get(4)?,
        groups: if groups.is_empty() {
            vec![]
        } else {
            groups.split(',').map(str::to_string).collect()
        },
        repost_count: r.get(6)?,
        deferred_count: r.get(7)?,
        state: r.get(8)?,
        next_attempt_at: parse_time(&r.get::<_, String>(9)?)?,
        lease_owner: r.get(10)?,
        lease_expires_at: parse_ptr(r.get(11)?)?,
        last_error: r.get(12)?,
        last_checked_at: parse_ptr(r.get(13)?)?,
    })
}

/// Durable access to transfer files and verification failures.
#[derive(Clone)]
pub struct Store {
    db: Db,
}

impl Store {
    /// The `transfer_files` and `verification_failures` tables must exist (migration 007).
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    fn files(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<TransferFile>> {
        let c = self.db.lock();
        let mut stmt = c.prepare(sql)?;
        let mut rows = stmt.query(args)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(scan_file(r)?);
        }
        Ok(out)
    }

    /// Inserts or replaces a `transfer_files` row, refreshing `updated_at`.
    pub fn upsert_file(&self, f: &TransferFile) -> Result<()> {
        let manifest_version = if f.manifest_version == 0 {
            1
        } else {
            f.manifest_version
        };
        let upload = if f.upload_state.is_empty() {
            STATE_PLANNED
        } else {
            &f.upload_state
        };
        let verification = if f.verification_state.is_empty() {
            STATE_PLANNED
        } else {
            &f.verification_state
        };
        self.db
            .lock()
            .execute(
                "INSERT INTO transfer_files (
                    transfer_id, file_id, completed_item_id, manifest_path, manifest_version,
                    source_path, file_role, article_count, upload_state, verification_state,
                    posted_at, next_check_at, cleanup_policy, last_error, updated_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                ON CONFLICT(transfer_id, file_id) DO UPDATE SET
                    completed_item_id = excluded.completed_item_id,
                    manifest_path     = excluded.manifest_path,
                    manifest_version  = excluded.manifest_version,
                    source_path       = excluded.source_path,
                    file_role         = excluded.file_role,
                    article_count     = excluded.article_count,
                    upload_state      = excluded.upload_state,
                    verification_state= excluded.verification_state,
                    posted_at         = excluded.posted_at,
                    next_check_at     = excluded.next_check_at,
                    cleanup_policy    = excluded.cleanup_policy,
                    last_error        = excluded.last_error,
                    updated_at        = excluded.updated_at",
                params![
                    f.transfer_id,
                    f.file_id,
                    null_str(&f.completed_item_id),
                    f.manifest_path,
                    manifest_version,
                    f.source_path,
                    f.file_role,
                    f.article_count,
                    upload,
                    verification,
                    fmt_ptr(f.posted_at),
                    fmt_ptr(f.next_check_at),
                    f.cleanup_policy,
                    f.last_error,
                    fmt_time(Utc::now()),
                ],
            )
            .map(|_| ())
            .map_err(wrap("upsert transfer file"))
    }

    /// A single transfer file, or `None` when absent (Go: `sql.ErrNoRows`).
    pub fn get_file(&self, transfer_id: &str, file_id: &str) -> Result<Option<TransferFile>> {
        Ok(self
            .files(&format!("SELECT {TRANSFER_FILE_COLS} FROM transfer_files WHERE transfer_id = ?1 AND file_id = ?2"), params![transfer_id, file_id])?
            .into_iter()
            .next())
    }

    pub fn list_files_by_transfer(&self, transfer_id: &str) -> Result<Vec<TransferFile>> {
        self.files(&format!("SELECT {TRANSFER_FILE_COLS} FROM transfer_files WHERE transfer_id = ?1 ORDER BY file_id"), params![transfer_id])
    }

    /// Every distinct source path still tracked by a transfer, whatever its state.
    pub fn list_source_paths(&self) -> Result<Vec<String>> {
        let c = self.db.lock();
        let mut stmt = c.prepare("SELECT DISTINCT source_path FROM transfer_files")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn set_verification_state(
        &self,
        transfer_id: &str,
        file_id: &str,
        state: &str,
        next_check_at: Option<DateTime<Utc>>,
        last_err: &str,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE transfer_files SET verification_state = ?1, next_check_at = ?2, last_error = ?3, updated_at = ?4
             WHERE transfer_id = ?5 AND file_id = ?6",
            params![state, fmt_ptr(next_check_at), last_err, fmt_time(Utc::now()), transfer_id, file_id],
        )?;
        Ok(())
    }

    /// Reschedules a file's first verification check after a failed attempt.
    pub fn defer_file_check(
        &self,
        transfer_id: &str,
        file_id: &str,
        next_check_at: DateTime<Utc>,
        last_err: &str,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE transfer_files SET check_attempts = check_attempts + 1, next_check_at = ?1, last_error = ?2, updated_at = ?3
             WHERE transfer_id = ?4 AND file_id = ?5",
            params![fmt_time(next_check_at), last_err, fmt_time(Utc::now()), transfer_id, file_id],
        )?;
        Ok(())
    }

    /// Records that a file finished uploading and when its first verification check is due.
    pub fn mark_uploaded(
        &self,
        transfer_id: &str,
        file_id: &str,
        posted_at: DateTime<Utc>,
        next_check_at: DateTime<Utc>,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE transfer_files SET upload_state = ?1, verification_state = ?2, posted_at = ?3, next_check_at = ?4, updated_at = ?5
             WHERE transfer_id = ?6 AND file_id = ?7",
            params![STATE_UPLOADED, STATE_UPLOADED, fmt_time(posted_at), fmt_time(next_check_at), fmt_time(Utc::now()), transfer_id, file_id],
        )?;
        Ok(())
    }

    /// Marks every file of a transfer uploaded and, when `original_policy` is set, records that
    /// cleanup policy on the original files; one transaction, so a crash cannot leave a transfer
    /// half-marked and the commit costs one fsync instead of one per file.
    pub fn mark_transfer_uploaded(
        &self,
        transfer_id: &str,
        posted_at: DateTime<Utc>,
        next_check_at: DateTime<Utc>,
        original_policy: Option<&str>,
        original_role: &str,
    ) -> Result<()> {
        let mut c = self.db.lock();
        let tx = c.transaction()?;
        let now = fmt_time(Utc::now());
        tx.execute(
            "UPDATE transfer_files SET upload_state = ?1, verification_state = ?2, posted_at = ?3, next_check_at = ?4, updated_at = ?5
             WHERE transfer_id = ?6",
            params![STATE_UPLOADED, STATE_UPLOADED, fmt_time(posted_at), fmt_time(next_check_at), now, transfer_id],
        )?;
        if let Some(policy) = original_policy {
            tx.execute(
                "UPDATE transfer_files SET cleanup_policy = ?1, updated_at = ?2 WHERE transfer_id = ?3 AND file_role = ?4",
                params![policy, now, transfer_id, original_role],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_cleanup_policy(&self, transfer_id: &str, file_id: &str, policy: &str) -> Result<()> {
        self.db.lock().execute(
            "UPDATE transfer_files SET cleanup_policy = ?1, updated_at = ?2 WHERE transfer_id = ?3 AND file_id = ?4",
            params![policy, fmt_time(Utc::now()), transfer_id, file_id],
        )?;
        Ok(())
    }

    /// Links a transfer's files to the `completed_items` row created for the upload.
    pub fn set_completed_item_for_transfer(
        &self,
        transfer_id: &str,
        completed_item_id: &str,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE transfer_files SET completed_item_id = ?1, updated_at = ?2 WHERE transfer_id = ?3",
            params![completed_item_id, fmt_time(Utc::now()), transfer_id],
        )?;
        Ok(())
    }

    /// Updates the user-facing queue row's verification status; an empty id is a no-op.
    pub fn set_completed_item_verification_status(
        &self,
        completed_item_id: &str,
        status: &str,
    ) -> Result<()> {
        if completed_item_id.is_empty() {
            return Ok(());
        }
        self.db.lock().execute(
            "UPDATE completed_items SET verification_status = ?1 WHERE id = ?2",
            params![status, completed_item_id],
        )?;
        Ok(())
    }

    /// Moves `pending_article_checks` rows into `verification_failures` as STAT-only records, in one
    /// transaction. Idempotent, and a no-op when the legacy table does not exist.
    pub fn migrate_legacy_pending_checks(&self) -> Result<usize> {
        let mut c = self.db.lock();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending: Vec<(String, String, String, String, i64)> = {
            let Ok(mut stmt) = tx.prepare("SELECT completed_item_id, message_id, groups, next_retry_at, retry_count FROM pending_article_checks") else {
                return Ok(0); // legacy table absent (fresh install)
            };
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for (item, message_id, groups, next_retry_at, retry_count) in &pending {
            let next = parse_time(next_retry_at).unwrap_or_else(|_| Utc::now());
            tx.execute(
                "INSERT INTO verification_failures
                    (transfer_id, file_id, article_index, message_id, groups, deferred_count, state, next_attempt_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(transfer_id, file_id, message_id) DO NOTHING",
                params![item, LEGACY_FILE_ID, -1, message_id, groups, retry_count, FAILURE_PENDING, fmt_time(next)],
            )
            .map_err(wrap("migrate legacy check"))?;
        }
        tx.execute("DELETE FROM pending_article_checks", [])?;
        tx.commit()?;
        Ok(pending.len())
    }

    /// The NZB path recorded for a completed item; empty id gives an empty path.
    pub fn get_completed_item_nzb_path(&self, completed_item_id: &str) -> Result<String> {
        if completed_item_id.is_empty() {
            return Ok(String::new());
        }
        Ok(self.db.lock().query_row(
            "SELECT nzb_path FROM completed_items WHERE id = ?1",
            params![completed_item_id],
            |r| r.get(0),
        )?)
    }

    pub fn delete_files_by_transfer(&self, transfer_id: &str) -> Result<()> {
        self.db.lock().execute(
            "DELETE FROM transfer_files WHERE transfer_id = ?1",
            params![transfer_id],
        )?;
        Ok(())
    }

    pub fn delete_failures_by_transfer(&self, transfer_id: &str) -> Result<()> {
        self.db.lock().execute(
            "DELETE FROM verification_failures WHERE transfer_id = ?1",
            params![transfer_id],
        )?;
        Ok(())
    }

    /// Files awaiting their first verification check, oldest first.
    pub fn list_due_files(&self, now: DateTime<Utc>, limit: i64) -> Result<Vec<TransferFile>> {
        self.files(
            &format!(
                "SELECT {TRANSFER_FILE_COLS} FROM transfer_files
                 WHERE verification_state = ?1 AND next_check_at IS NOT NULL AND next_check_at <= ?2
                 ORDER BY next_check_at LIMIT ?3"
            ),
            params![STATE_UPLOADED, fmt_time(now), limit],
        )
    }

    /// Updates a file's upload state and, when given, `posted_at`.
    pub fn set_upload_state(
        &self,
        transfer_id: &str,
        file_id: &str,
        state: &str,
        posted_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE transfer_files SET upload_state = ?1, posted_at = COALESCE(?2, posted_at), updated_at = ?3
             WHERE transfer_id = ?4 AND file_id = ?5",
            params![state, fmt_ptr(posted_at), fmt_time(Utc::now()), transfer_id, file_id],
        )?;
        Ok(())
    }

    /// Records a failed article; unique per (transfer, file, message id), duplicates are ignored.
    pub fn add_failure(&self, f: &VerificationFailure) -> Result<()> {
        self.add_failures(std::slice::from_ref(f))
    }

    /// Inserts every failure in one transaction (one lock acquisition and one commit), idempotently.
    pub fn add_failures(&self, failures: &[VerificationFailure]) -> Result<()> {
        if failures.is_empty() {
            return Ok(());
        }
        let mut c = self.db.lock();
        let tx = c.transaction().map_err(wrap("add verification failures"))?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO verification_failures (
                        transfer_id, file_id, article_index, message_id, groups,
                        repost_count, deferred_count, state, next_attempt_at, last_error
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                     ON CONFLICT(transfer_id, file_id, message_id) DO NOTHING",
                )
                .map_err(wrap("add verification failures"))?;
            for f in failures {
                let state = if f.state.is_empty() {
                    FAILURE_PENDING
                } else {
                    &f.state
                };
                let next = if f.next_attempt_at == DateTime::<Utc>::MIN_UTC {
                    Utc::now()
                } else {
                    f.next_attempt_at
                };
                stmt.execute(params![
                    f.transfer_id,
                    f.file_id,
                    f.article_index,
                    f.message_id,
                    f.groups.join(","),
                    f.repost_count,
                    f.deferred_count,
                    state,
                    fmt_time(next),
                    f.last_error
                ])
                .map_err(wrap("add verification failures"))?;
            }
        }
        tx.commit().map_err(wrap("add verification failures"))
    }

    /// Atomically leases up to `limit` pending failures that are due and not leased (or whose lease
    /// expired) to `owner` until `now + lease`. The transaction is IMMEDIATE so concurrent workers
    /// or processes never claim the same row.
    pub fn claim_due_failures(
        &self,
        owner: &str,
        lease: chrono::Duration,
        limit: i64,
        now: DateTime<Utc>,
    ) -> Result<Vec<VerificationFailure>> {
        let mut c = self.db.lock();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now_s = fmt_time(now);
        let mut claimed = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {FAILURE_COLS} FROM verification_failures
                 WHERE state = ?1 AND next_attempt_at <= ?2
                   AND (lease_owner = '' OR lease_expires_at IS NULL OR lease_expires_at <= ?3)
                 ORDER BY next_attempt_at LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![FAILURE_PENDING, now_s, now_s, limit])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push(scan_failure(r)?);
            }
            out
        };
        if claimed.is_empty() {
            tx.commit()?;
            return Ok(claimed);
        }
        let expires = fmt_time(now + lease);
        let expires_t = parse_time(&expires)?;
        for f in &mut claimed {
            tx.execute("UPDATE verification_failures SET lease_owner = ?1, lease_expires_at = ?2 WHERE id = ?3", params![owner, expires, f.id])?;
            f.lease_owner = owner.to_string();
            f.lease_expires_at = Some(expires_t);
        }
        tx.commit()?;
        Ok(claimed)
    }

    /// Records the outcome of a check/re-post attempt and releases the lease.
    pub fn update_failure_after_check(&self, f: &VerificationFailure) -> Result<()> {
        self.db.lock().execute(
            "UPDATE verification_failures
             SET state = ?1, repost_count = ?2, deferred_count = ?3, next_attempt_at = ?4,
                 last_error = ?5, last_checked_at = ?6, lease_owner = '', lease_expires_at = NULL
             WHERE id = ?7",
            params![
                f.state,
                f.repost_count,
                f.deferred_count,
                fmt_time(f.next_attempt_at),
                f.last_error,
                fmt_time(Utc::now()),
                f.id
            ],
        )?;
        Ok(())
    }

    /// Clears leases whose expiry has passed so work can be picked up again after a crash.
    pub fn reclaim_expired_leases(&self, now: DateTime<Utc>) -> Result<i64> {
        let n = self.db.lock().execute(
            "UPDATE verification_failures SET lease_owner = '', lease_expires_at = NULL
             WHERE lease_owner != '' AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?1",
            params![fmt_time(now)],
        )?;
        Ok(n as i64)
    }

    /// Ids of completed items whose verification status is still `pending_verification`.
    pub fn list_pending_verification_item_ids(&self) -> Result<Vec<String>> {
        let c = self.db.lock();
        let mut stmt = c.prepare(
            "SELECT id FROM completed_items WHERE verification_status = 'pending_verification'",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn list_files_by_completed_item(
        &self,
        completed_item_id: &str,
    ) -> Result<Vec<TransferFile>> {
        self.files(&format!("SELECT {TRANSFER_FILE_COLS} FROM transfer_files WHERE completed_item_id = ?1 ORDER BY file_id"), params![completed_item_id])
    }

    /// Failure rows for a transfer file in `state` (`""` = any state).
    pub fn count_failures(&self, transfer_id: &str, file_id: &str, state: &str) -> Result<i64> {
        let c = self.db.lock();
        let n = if state.is_empty() {
            c.query_row("SELECT COUNT(*) FROM verification_failures WHERE transfer_id = ?1 AND file_id = ?2", params![transfer_id, file_id], |r| r.get(0))
        } else {
            c.query_row(
                "SELECT COUNT(*) FROM verification_failures WHERE transfer_id = ?1 AND file_id = ?2 AND state = ?3",
                params![transfer_id, file_id, state],
                |r| r.get(0),
            )
        };
        Ok(n.optional()?.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::database::MigrationRunner;

    fn store() -> Store {
        let db = Db::in_memory().unwrap();
        db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
        MigrationRunner::new(db.clone()).migrate_up().unwrap();
        Store::new(db)
    }

    fn file(t: &str, f: &str) -> TransferFile {
        TransferFile {
            transfer_id: t.into(),
            file_id: f.into(),
            manifest_path: "m".into(),
            source_path: "s".into(),
            file_role: "original".into(),
            ..Default::default()
        }
    }

    fn insert_completed_item(s: &Store, id: &str) {
        s.db.lock()
            .execute(
                "INSERT INTO completed_items (id, path, size, nzb_path, created_at, job_data, verification_status) VALUES (?1,'/d/a',1,'/o/a.nzb','2026-01-01T00:00:00Z','{}','pending_verification')",
                params![id],
            )
            .unwrap();
    }

    #[test]
    fn time_format_matches_go_rfc3339nano() {
        let t = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert_eq!(fmt_time(t), "2023-11-14T22:13:20Z");
        assert_eq!(
            fmt_time(t + Duration::milliseconds(120)),
            "2023-11-14T22:13:20.12Z"
        );
        assert_eq!(
            fmt_time(t + Duration::nanoseconds(123_456_789)),
            "2023-11-14T22:13:20.123456789Z"
        );
        assert_eq!(
            parse_time("2023-11-14T22:13:20.120Z").unwrap(),
            t + Duration::milliseconds(120)
        );
        assert!(parse_time("garbage").is_err());
    }

    #[test]
    fn upsert_and_get_file_updates_in_place() {
        let s = store();
        let posted = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut f = TransferFile {
            manifest_path: "/m/tid-1/fid-1.jsonl.zst".into(),
            source_path: "/data/a.mkv".into(),
            article_count: 42,
            upload_state: STATE_UPLOADED.into(),
            posted_at: Some(posted),
            cleanup_policy: "delete_original".into(),
            ..file("tid-1", "fid-1")
        };
        s.upsert_file(&f).unwrap();
        let got = s.get_file("tid-1", "fid-1").unwrap().unwrap();
        assert_eq!(
            (
                got.article_count,
                got.upload_state.as_str(),
                got.source_path.as_str()
            ),
            (42, STATE_UPLOADED, "/data/a.mkv")
        );
        assert_eq!(got.posted_at, Some(posted));
        assert_eq!(
            (got.verification_state.as_str(), got.manifest_version),
            (STATE_PLANNED, 1)
        );
        f.article_count = 99;
        s.upsert_file(&f).unwrap();
        let files = s.list_files_by_transfer("tid-1").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].article_count, 99);
        assert_eq!(s.get_file("nope", "nope").unwrap(), None);
    }

    #[test]
    fn set_states_and_defer() {
        let s = store();
        s.upsert_file(&file("t", "f")).unwrap();
        let next = Utc::now() + Duration::hours(1);
        s.set_verification_state("t", "f", STATE_VERIFYING, Some(next), "boom")
            .unwrap();
        let got = s.get_file("t", "f").unwrap().unwrap();
        assert_eq!(
            (got.verification_state.as_str(), got.last_error.as_str()),
            (STATE_VERIFYING, "boom")
        );
        assert!((got.next_check_at.unwrap() - next).abs() < Duration::seconds(1));
        s.defer_file_check("t", "f", next, "retry").unwrap();
        s.defer_file_check("t", "f", next, "retry").unwrap();
        assert_eq!(s.get_file("t", "f").unwrap().unwrap().check_attempts, 2);
        s.set_cleanup_policy("t", "f", CLEANUP_DELETE_ORIGINAL)
            .unwrap();
        s.set_upload_state("t", "f", STATE_UPLOADING, None).unwrap();
        let got = s.get_file("t", "f").unwrap().unwrap();
        assert_eq!(
            (
                got.cleanup_policy.as_str(),
                got.upload_state.as_str(),
                got.posted_at
            ),
            (CLEANUP_DELETE_ORIGINAL, STATE_UPLOADING, None)
        );
    }

    #[test]
    fn add_failure_is_idempotent() {
        let s = store();
        let f = VerificationFailure {
            transfer_id: "t".into(),
            file_id: "f".into(),
            article_index: 3,
            message_id: "<a@p>".into(),
            groups: vec!["g1".into(), "g2".into()],
            ..Default::default()
        };
        s.add_failure(&f).unwrap();
        s.add_failure(&f).unwrap();
        assert_eq!(s.count_failures("t", "f", "").unwrap(), 1);
    }

    fn failure(id: &str, next: DateTime<Utc>) -> VerificationFailure {
        VerificationFailure {
            transfer_id: "t".into(),
            file_id: "f".into(),
            message_id: id.into(),
            next_attempt_at: next,
            ..Default::default()
        }
    }

    #[test]
    fn claim_due_failures_leasing() {
        let s = store();
        let now = Utc::now();
        s.add_failure(&failure("<1>", now - Duration::minutes(1)))
            .unwrap();
        s.add_failure(&failure("<2>", now - Duration::minutes(1)))
            .unwrap();
        s.add_failure(&failure("<3>", now + Duration::hours(1)))
            .unwrap();
        assert_eq!(
            s.claim_due_failures("worker-1", Duration::minutes(5), 10, now)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            s.claim_due_failures("worker-2", Duration::minutes(5), 10, now)
                .unwrap()
                .len(),
            0,
            "leases held"
        );
        assert_eq!(
            s.reclaim_expired_leases(now + Duration::minutes(10))
                .unwrap(),
            2
        );
        assert_eq!(
            s.claim_due_failures(
                "worker-2",
                Duration::minutes(1),
                10,
                now + Duration::minutes(10)
            )
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn update_failure_after_check_resolves() {
        let s = store();
        let now = Utc::now();
        s.add_failure(&failure("<1>", now - Duration::minutes(1)))
            .unwrap();
        let mut claimed = s
            .claim_due_failures("w", Duration::minutes(1), 10, now)
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let mut upd = claimed.remove(0);
        upd.state = FAILURE_RESOLVED.into();
        upd.repost_count = 1;
        upd.next_attempt_at = now;
        s.update_failure_after_check(&upd).unwrap();
        assert_eq!(s.count_failures("t", "f", FAILURE_PENDING).unwrap(), 0);
        assert_eq!(s.count_failures("t", "f", FAILURE_RESOLVED).unwrap(), 1);
    }

    #[test]
    fn mark_uploaded_and_list_due_files() {
        let s = store();
        let now = Utc::now();
        for fid in ["f-past", "f-future"] {
            s.upsert_file(&TransferFile {
                article_count: 1,
                ..file("t", fid)
            })
            .unwrap();
        }
        let posted = now - Duration::hours(1);
        s.mark_uploaded("t", "f-past", posted, now - Duration::minutes(1))
            .unwrap();
        s.mark_uploaded("t", "f-future", posted, now + Duration::hours(1))
            .unwrap();
        let past = s.get_file("t", "f-past").unwrap().unwrap();
        assert_eq!(
            (past.upload_state.as_str(), past.verification_state.as_str()),
            (STATE_UPLOADED, STATE_UPLOADED)
        );
        assert!((past.posted_at.unwrap() - posted).abs() < Duration::milliseconds(1));
        let due = s.list_due_files(now, 10).unwrap();
        assert_eq!(
            due.iter().map(|f| f.file_id.as_str()).collect::<Vec<_>>(),
            ["f-past"]
        );
        assert_eq!(
            s.list_due_files(now + Duration::hours(2), 10)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn completed_item_links_and_status() {
        let s = store();
        for fid in ["f1", "f2"] {
            s.upsert_file(&file("t", fid)).unwrap();
        }
        s.set_completed_item_for_transfer("t", "ci-1").unwrap();
        assert!(
            s.list_files_by_transfer("t")
                .unwrap()
                .iter()
                .all(|f| f.completed_item_id == "ci-1")
        );
        assert_eq!(s.list_files_by_completed_item("ci-1").unwrap().len(), 2);
        s.set_completed_item_verification_status("", "x").unwrap();
        insert_completed_item(&s, "ci-1");
        assert_eq!(s.list_pending_verification_item_ids().unwrap(), ["ci-1"]);
        s.set_completed_item_verification_status("ci-1", "verified")
            .unwrap();
        assert!(s.list_pending_verification_item_ids().unwrap().is_empty());
        assert_eq!(s.get_completed_item_nzb_path("ci-1").unwrap(), "/o/a.nzb");
        assert_eq!(s.get_completed_item_nzb_path("").unwrap(), "");
    }

    #[test]
    fn migrate_legacy_pending_checks_is_idempotent() {
        let s = store();
        insert_completed_item(&s, "ci-1");
        for mid in ["<a@p>", "<b@p>"] {
            s.db.lock()
                .execute(
                    "INSERT INTO pending_article_checks (completed_item_id, message_id, groups, next_retry_at, retry_count) VALUES ('ci-1',?1,'alt.binaries.test','2026-01-01T00:00:00Z',2)",
                    params![mid],
                )
                .unwrap();
        }
        assert_eq!(s.migrate_legacy_pending_checks().unwrap(), 2);
        let remaining: i64 =
            s.db.lock()
                .query_row("SELECT COUNT(*) FROM pending_article_checks", [], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(remaining, 0);
        let failures = s
            .claim_due_failures(
                "w",
                Duration::minutes(1),
                10,
                Utc::now() + Duration::hours(1),
            )
            .unwrap();
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().all(|f| f.transfer_id == "ci-1"
            && f.file_id == LEGACY_FILE_ID
            && f.article_index == -1
            && f.deferred_count == 2));
        assert_eq!(s.migrate_legacy_pending_checks().unwrap(), 0);
    }

    #[test]
    fn migrate_legacy_is_a_noop_without_the_table() {
        let db = Db::in_memory().unwrap();
        db.lock()
            .execute_batch("create table verification_failures (id integer)")
            .unwrap();
        assert_eq!(Store::new(db).migrate_legacy_pending_checks().unwrap(), 0);
    }

    #[test]
    fn list_source_paths_is_distinct_and_deletes_work() {
        let s = store();
        for (t, f, p) in [
            ("t1", "a", "/x/a.mkv"),
            ("t1", "b", "/x/a.mkv.par2"),
            ("t2", "c", "/x/a.mkv.par2"),
        ] {
            s.upsert_file(&TransferFile {
                source_path: p.into(),
                ..file(t, f)
            })
            .unwrap();
        }
        assert_eq!(s.list_source_paths().unwrap().len(), 2);
        s.delete_files_by_transfer("t1").unwrap();
        assert_eq!(s.list_source_paths().unwrap(), ["/x/a.mkv.par2"]);
        s.add_failure(&failure("<1>", Utc::now())).unwrap();
        s.delete_failures_by_transfer("t").unwrap();
        assert_eq!(s.count_failures("t", "f", "").unwrap(), 0);
    }

    /// A row written by the Go store (RFC3339Nano text) is read back, and ours sorts the same way.
    #[test]
    fn reads_rows_in_the_go_timestamp_format() {
        let s = store();
        s.db.lock()
            .execute(
                "INSERT INTO transfer_files (transfer_id,file_id,manifest_path,source_path,verification_state,next_check_at) VALUES ('t','g','m','s','uploaded','2026-01-02T03:04:05.5Z')",
                [],
            )
            .unwrap();
        let g = s.get_file("t", "g").unwrap().unwrap();
        assert_eq!(
            g.next_check_at,
            DateTime::from_timestamp(1_767_323_045, 500_000_000)
        );
    }
}
