//! The first check of uploaded files: stream each manifest and STAT every article through a
//! sliding window, persisting only the misses. Several files are swept at once; they all draw from
//! the stater's shared in-flight limit, so this adds overlap, not load.

use std::io;

use futures_util::stream::{self, FuturesUnordered, StreamExt};
use tokio_util::sync::CancellationToken;

use super::{LiveGuard, Result, Service, VerifyError, file_name, log};
use crate::manifest::{ArticleRecord, ManifestError, Reader};
use crate::transferstore::{self as ts, TransferFile, VerificationFailure};

/// Files swept concurrently. Small files rarely fill the STAT window on their own.
const FILE_PARALLELISM: usize = 4;
/// Results between live progress updates.
const PROGRESS_EVERY: i64 = 64;

/// Why a sweep stopped before checking every article.
enum Stop {
    Canceled,
    /// Unreadable manifest content: back off, spending the file's check budget.
    Manifest(String),
    /// Every STAT failed, so the server state is unknown: retry later without spending budget
    /// (a silent server is not proof of loss, the same rule the failure rechecks follow).
    Unanswered(String),
}

impl Service {
    /// First check for every file whose check is due, up to `limit`, [`FILE_PARALLELISM`] at a
    /// time. A failure on one file is logged and does not abort the batch. Returns the number
    /// verified.
    pub async fn verify_due_files(
        &self,
        cancel: &CancellationToken,
        now: chrono::DateTime<chrono::Utc>,
        limit: i64,
    ) -> Result<usize> {
        let due = self.store.list_due_files(now, limit)?;
        let processed = stream::iter(due)
            .map(|tf| async move {
                match self.verify_file(cancel, &tf).await {
                    Ok(()) => 1,
                    Err(VerifyError::Canceled) => 0,
                    Err(e) => {
                        log(&format!(
                            "verify file failed: transfer={} file={} error={e}",
                            tf.transfer_id, tf.file_id
                        ));
                        0
                    }
                }
            })
            .buffer_unordered(FILE_PARALLELISM)
            .fold(0usize, |n, ok| async move { n + ok })
            .await;
        if cancel.is_cancelled() {
            return Err(VerifyError::Canceled);
        }
        Ok(processed)
    }

    /// STATs every article of a file's manifest and persists only the misses. No misses: verified.
    /// Otherwise `verifying`, with the misses left for `process_due_failures`.
    pub async fn verify_file(&self, cancel: &CancellationToken, tf: &TransferFile) -> Result<()> {
        let mut reader = match Reader::open(std::path::Path::new(&tf.manifest_path)) {
            Ok(r) => r,
            Err(ManifestError::Open(e)) if e.kind() == io::ErrorKind::NotFound => {
                // The manifest will never come back: fail the file instead of retrying forever, or
                // the completed item stays pending_verification across restarts (issue #168).
                return self
                    .fail_file_terminally(tf, &format!("manifest missing: {e}"))
                    .await;
            }
            Err(e) => return self.defer_file_check(tf, &e.to_string()).await,
        };
        crate::logx::info(
            "Verifying upload: checking every article is on the server",
            &[
                ("file", file_name(&tf.source_path).into()),
                ("articles", tf.article_count.into()),
            ],
        );
        let missing = match self.sweep(cancel, tf, &mut reader).await {
            Ok(m) => m,
            Err(Stop::Canceled) => return Err(VerifyError::Canceled),
            Err(Stop::Manifest(e)) => return self.defer_file_check(tf, &e).await,
            Err(Stop::Unanswered(e)) => return self.retry_unanswered(tf, &e),
        };
        if missing.is_empty() {
            self.store.set_verification_state(
                &tf.transfer_id,
                &tf.file_id,
                ts::STATE_VERIFIED,
                None,
                "",
            )?;
            crate::logx::info(
                "File verified: every article is on the server",
                &[
                    ("file", file_name(&tf.source_path).into()),
                    ("articles", tf.article_count.into()),
                ],
            );
            self.finalize_transfer(&tf.transfer_id).await;
            return Ok(());
        }
        self.record_misses(tf, &missing)
    }

    /// Checks every record, keeping up to the stater's window of STATs in flight. Articles the
    /// early checker already confirmed are counted without a STAT. A STAT error counts as missing
    /// (the per-failure recheck re-validates before spending any budget), unless every STAT failed:
    /// then the server state is unknown and the whole file is retried later instead of flooding the
    /// failure table with one row per article.
    async fn sweep(
        &self,
        cancel: &CancellationToken,
        tf: &TransferFile,
        reader: &mut Reader,
    ) -> std::result::Result<Vec<ArticleRecord>, Stop> {
        let live = LiveGuard {
            live: &self.live,
            key: (tf.transfer_id.clone(), tf.file_id.clone()),
        };
        live.set(0, 0);
        let window = self.stater.window().max(1);
        let mut inflight = FuturesUnordered::new();
        let mut missing: Vec<ArticleRecord> = Vec::new();
        let (mut checked, mut answered, mut errored) = (0i64, 0i64, 0i64);
        let mut exhausted = false;
        loop {
            while !exhausted && inflight.len() < window {
                if cancel.is_cancelled() {
                    return Err(Stop::Canceled);
                }
                match reader.next_record() {
                    Ok(Some(rec)) => {
                        let early = self.early.as_deref();
                        let pending = early.is_some_and(|e| e.is_pending(&rec.message_id));
                        if !pending && early.is_some_and(|e| e.take_confirmed(&rec.message_id)) {
                            checked += 1;
                            answered += 1;
                            continue;
                        }
                        let stater = &self.stater;
                        inflight.push(async move {
                            // posted moments ago: its own check is due soon, wait for it rather
                            // than STAT before the propagation delay has passed
                            if pending
                                && let Some(e) = early
                                && e.settle(&rec.message_id).await
                            {
                                return (rec, Ok(false));
                            }
                            let r = stater.stat(&rec.message_id).await;
                            (rec, r)
                        });
                    }
                    Ok(None) => exhausted = true,
                    // unreadable or corrupt content: back off (and eventually terminalize)
                    Err(e) => return Err(Stop::Manifest(e.to_string())),
                }
            }
            let next = tokio::select! {
                () = cancel.cancelled() => return Err(Stop::Canceled),
                r = inflight.next() => r,
            };
            let Some((rec, r)) = next else { break };
            checked += 1;
            match r {
                Ok(false) => answered += 1,
                Ok(true) => {
                    answered += 1;
                    missing.push(rec);
                }
                Err(_) => {
                    errored += 1;
                    missing.push(rec);
                }
            }
            if checked % PROGRESS_EVERY == 0 {
                live.set(checked, missing.len() as i64);
            }
        }
        if answered == 0 && errored > 0 {
            return Err(Stop::Unanswered(format!(
                "every STAT check failed ({errored} articles)"
            )));
        }
        missing.sort_by_key(|r| r.index);
        Ok(missing)
    }

    /// Reschedules the first check after the base backoff, keeping the file `uploaded` and its
    /// check-attempt count untouched.
    fn retry_unanswered(&self, tf: &TransferFile, cause: &str) -> Result<()> {
        let next = self.now() + self.cfg.deferred_backoff;
        log(&format!(
            "check postponed, no STAT answered: transfer={} file={} next_check_at={next} error={cause}",
            tf.transfer_id, tf.file_id
        ));
        self.store.set_verification_state(
            &tf.transfer_id,
            &tf.file_id,
            ts::STATE_UPLOADED,
            Some(next),
            cause,
        )?;
        Ok(())
    }

    /// Persists a sweep's misses for durable re-posting/rechecking and leaves the file `verifying`.
    fn record_misses(&self, tf: &TransferFile, missing: &[ArticleRecord]) -> Result<()> {
        let next = self.now() + self.cfg.propagation_delay;
        let failures: Vec<VerificationFailure> = missing
            .iter()
            .map(|rec| VerificationFailure {
                transfer_id: tf.transfer_id.clone(),
                file_id: tf.file_id.clone(),
                article_index: rec.index,
                message_id: rec.message_id.clone(),
                groups: rec.groups.clone(),
                state: ts::FAILURE_PENDING.into(),
                next_attempt_at: next,
                ..Default::default()
            })
            .collect();
        // one transaction instead of a commit per missing article, off the async worker thread
        crate::blocking::in_place(|| self.store.add_failures(&failures))?;
        crate::logx::warn(
            "Articles missing after upload; they will be rechecked and re-posted",
            &[
                ("file", file_name(&tf.source_path).into()),
                ("missing", missing.len().into()),
                ("articles", tf.article_count.into()),
            ],
        );
        self.store.set_verification_state(
            &tf.transfer_id,
            &tf.file_id,
            ts::STATE_VERIFYING,
            Some(next),
            "",
        )?;
        Ok(())
    }
}
