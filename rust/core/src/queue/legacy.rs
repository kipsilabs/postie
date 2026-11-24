//! The legacy deferred article-check path (`pending_article_checks`), kept for databases written
//! before the durable transfer architecture.

use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{Queue, QueueError, Result, ms};
use crate::transferstore::parse_time;

#[derive(Debug, Clone, PartialEq)]
pub struct PendingArticleCheck {
    pub id: i64,
    pub completed_item_id: String,
    pub message_id: String,
    /// JSON-encoded array of group names.
    pub groups: String,
    /// pending, verified or failed.
    pub status: String,
    pub retry_count: i64,
    pub next_retry_at: DateTime<Utc>,
    pub first_failure_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Queue {
    /// Inserts pending checks for a completed item in one transaction.
    pub fn add_pending_article_checks(
        &self,
        completed_item_id: &str,
        articles: &[PendingArticleCheck],
    ) -> Result<()> {
        let mut conn = self.db.lock();
        let tx = conn
            .transaction()
            .map_err(|e| QueueError::Msg(format!("failed to begin transaction: {e}")))?;
        {
            let mut stmt = tx.prepare("INSERT INTO pending_article_checks (completed_item_id, message_id, groups, status, next_retry_at) VALUES (?1, ?2, ?3, 'pending', ?4)")?;
            for a in articles {
                stmt.execute(params![
                    completed_item_id,
                    a.message_id,
                    a.groups,
                    ms(a.next_retry_at)
                ])
                .map_err(|e| {
                    QueueError::Msg(format!("failed to insert pending article check: {e}"))
                })?;
            }
        }
        Ok(tx.commit()?)
    }

    /// Pending checks that are due, oldest first.
    pub fn get_articles_for_check(&self, limit: i64) -> Result<Vec<PendingArticleCheck>> {
        let conn = self.db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, completed_item_id, message_id, groups, status, retry_count, next_retry_at, first_failure_at, last_checked_at, created_at
                 FROM pending_article_checks WHERE status = 'pending' AND next_retry_at <= ?1 ORDER BY next_retry_at ASC LIMIT ?2",
            )
            .map_err(|e| QueueError::Msg(format!("failed to query pending article checks: {e}")))?;
        let rows = stmt.query_map(params![ms(Utc::now()), limit], |r| {
            let t = |s: String| parse_time(&s).unwrap_or_else(|_| super::zero_time());
            Ok(PendingArticleCheck {
                id: r.get(0)?,
                completed_item_id: r.get(1)?,
                message_id: r.get(2)?,
                groups: r.get(3)?,
                status: r.get(4)?,
                retry_count: r.get(5)?,
                next_retry_at: t(r.get(6)?),
                first_failure_at: t(r.get(7)?),
                last_checked_at: r.get::<_, Option<String>>(8)?.map(t),
                created_at: t(r.get(9)?),
            })
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }

    fn set_check_status(&self, id: i64, status: &str) -> Result<()> {
        self.db.lock().execute(
            "UPDATE pending_article_checks SET status = ?1, last_checked_at = ?2 WHERE id = ?3",
            params![status, ms(Utc::now()), id],
        )?;
        Ok(())
    }

    pub fn mark_article_verified(&self, id: i64) -> Result<()> {
        self.set_check_status(id, "verified")
    }

    pub fn mark_article_check_failed(&self, id: i64) -> Result<()> {
        self.set_check_status(id, "failed")
    }

    pub fn update_article_check_retry(
        &self,
        id: i64,
        retry_count: i64,
        next_retry_at: DateTime<Utc>,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE pending_article_checks SET retry_count = ?1, next_retry_at = ?2, last_checked_at = ?3 WHERE id = ?4",
            params![retry_count, ms(next_retry_at), ms(Utc::now()), id],
        )?;
        Ok(())
    }

    pub fn update_completed_item_verification_status(
        &self,
        completed_item_id: &str,
        status: &str,
    ) -> Result<()> {
        self.db.lock().execute(
            "UPDATE completed_items SET verification_status = ?1 WHERE id = ?2",
            params![status, completed_item_id],
        )?;
        Ok(())
    }

    /// `(total, pending, failed)` of the legacy checks for a completed item.
    pub fn get_pending_check_count_for_item(
        &self,
        completed_item_id: &str,
    ) -> Result<(i64, i64, i64)> {
        Ok(self.db.lock().query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END), 0), COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0)
             FROM pending_article_checks WHERE completed_item_id = ?1",
            params![completed_item_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    }
}
