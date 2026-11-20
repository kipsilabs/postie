//! Paginated queue listing: the same SQL, sort mapping and bucket ordering as Go's `GetQueueItems`.

use chrono::{DateTime, Utc};
use rusqlite::{Row, params, params_from_iter};
use serde::{Deserialize, Serialize, Serializer};

use super::{
    FileJob, Queue, QueueError, Result, STATUS_COMPLETE, STATUS_ERROR, STATUS_PENDING,
    STATUS_RUNNING, file_name_of, lenient_time, msg,
};
use crate::transferstore::{fmt_time, parse_time};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaginationParams {
    pub page: i64,
    pub limit: i64,
    #[serde(rename = "sortBy")]
    pub sort_by: String,
    pub order: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedResult {
    pub items: Vec<QueueItem>,
    pub total_items: i64,
    pub total_pages: i64,
    pub current_page: i64,
    pub items_per_page: i64,
    pub has_next: bool,
    pub has_prev: bool,
}

fn ser_time<S: Serializer>(t: &DateTime<Utc>, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&fmt_time(*t))
}

fn ser_opt_time<S: Serializer>(
    t: &Option<DateTime<Utc>>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match t {
        Some(t) => s.serialize_str(&fmt_time(*t)),
        None => s.serialize_none(),
    }
}

/// One row of the queue listing (same JSON as Go's `QueueItem`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: String,
    pub path: String,
    pub file_name: String,
    pub size: i64,
    pub status: String,
    pub retry_count: i64,
    pub priority: i64,
    pub error_message: Option<String>,
    #[serde(serialize_with = "ser_time")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ser_time")]
    pub updated_at: DateTime<Utc>,
    #[serde(serialize_with = "ser_opt_time")]
    pub completed_at: Option<DateTime<Utc>>,
    pub nzb_path: Option<String>,
    pub script_status: Option<String>,
    pub script_retry_count: i64,
    pub script_last_error: Option<String>,
    #[serde(serialize_with = "ser_opt_time")]
    pub script_next_retry_at: Option<DateTime<Utc>>,
    pub verification_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_articles: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_articles: Option<i64>,
}

fn blank() -> QueueItem {
    QueueItem {
        created_at: super::zero_time(),
        updated_at: super::zero_time(),
        ..Default::default()
    }
}

fn build_order_by(sort_by: &str, order: &str) -> String {
    let column = match sort_by {
        "priority" => "priority",
        "filename" => "file_name",
        "size" => "size",
        "status" => "status",
        _ => "created_at",
    };
    format!("{column} {}", if order == "asc" { "ASC" } else { "DESC" })
}

fn opt_t(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|s| parse_time(&s).ok())
}

/// Reads the completed-item column set shared by both completed queries.
fn completed_item(r: &Row<'_>) -> rusqlite::Result<QueueItem> {
    let path: String = r.get(1)?;
    let completed = lenient_time(&r.get::<_, String>(6)?);
    Ok(QueueItem {
        id: r.get(0)?,
        file_name: file_name_of(&path).to_string(),
        path,
        size: r.get(2)?,
        status: STATUS_COMPLETE.into(),
        priority: r.get(3)?,
        created_at: lenient_time(&r.get::<_, String>(5)?),
        updated_at: completed,
        completed_at: Some(completed),
        nzb_path: Some(r.get(4)?),
        script_status: r.get(7)?,
        script_retry_count: r.get(8)?,
        script_last_error: r.get(9)?,
        script_next_retry_at: opt_t(r.get(10)?),
        verification_status: r.get(11)?,
        ..blank()
    })
}

const COMPLETED_COLS: &str = "id, path, size, priority, nzb_path, created_at, completed_at, script_status, script_retry_count, script_last_error, script_next_retry_at, verification_status";

fn pending_item(r: &Row<'_>) -> Option<QueueItem> {
    let body: Vec<u8> = r.get(3).ok()?;
    let job: FileJob = serde_json::from_slice(&body).ok()?;
    Some(QueueItem {
        id: r.get(0).ok()?,
        file_name: file_name_of(&job.path).to_string(),
        path: job.path,
        size: job.size,
        status: STATUS_PENDING.into(),
        retry_count: job.retry_count,
        priority: job.priority,
        created_at: lenient_time(&r.get::<_, String>(1).ok()?),
        updated_at: lenient_time(&r.get::<_, String>(2).ok()?),
        ..blank()
    })
}

fn errored_item(r: &Row<'_>) -> Option<QueueItem> {
    let path: String = r.get(1).ok()?;
    let data: Vec<u8> = r.get(7).ok()?;
    let job: FileJob = serde_json::from_slice(&data).ok()?;
    Some(QueueItem {
        id: r.get(0).ok()?,
        file_name: file_name_of(&path).to_string(),
        path,
        size: r.get(2).ok()?,
        status: STATUS_ERROR.into(),
        retry_count: job.retry_count,
        priority: r.get(3).ok()?,
        error_message: Some(r.get(4).ok()?),
        created_at: lenient_time(&r.get::<_, String>(5).ok()?),
        updated_at: lenient_time(&r.get::<_, String>(6).ok()?),
        ..blank()
    })
}

fn running_item(r: &Row<'_>) -> Option<QueueItem> {
    let path: String = r.get(1).ok()?;
    Some(QueueItem {
        id: r.get(0).ok()?,
        file_name: path.clone(),
        path,
        size: r.get(2).ok()?,
        priority: r.get(3).ok()?,
        status: STATUS_RUNNING.into(),
        created_at: lenient_time(&r.get::<_, String>(4).ok()?),
        updated_at: lenient_time(&r.get::<_, String>(5).ok()?),
        ..blank()
    })
}

/// A merged-union row; `None` for a row Go's scan would reject.
fn merged_item(r: &Row<'_>) -> Option<QueueItem> {
    let path: String = r.get(1).ok()?;
    let file_name: String = r.get(11).ok()?;
    Some(QueueItem {
        id: r.get(0).ok()?,
        size: r.get(2).ok()?,
        priority: r.get(3).ok()?,
        status: r.get(4).ok()?,
        retry_count: r.get(5).ok()?,
        error_message: r.get(6).ok()?,
        created_at: lenient_time(&r.get::<_, String>(7).ok()?),
        updated_at: lenient_time(&r.get::<_, String>(8).ok()?),
        completed_at: opt_t(r.get(9).ok()?),
        nzb_path: r.get(10).ok()?,
        file_name: if file_name.is_empty() {
            file_name_of(&path).to_string()
        } else {
            file_name_of(&file_name).to_string()
        },
        path,
        script_status: r.get(12).ok()?,
        script_retry_count: r.get(13).ok()?,
        script_last_error: r.get(14).ok()?,
        script_next_retry_at: opt_t(r.get(15).ok()?),
        verification_status: r.get(16).ok()?,
        ..blank()
    })
}

const MERGED_SQL: &str = "
    SELECT id, path, size, priority, status, retry_count, error_message,
           created_at, updated_at, completed_at, nzb_path, file_name,
           script_status, script_retry_count, script_last_error, script_next_retry_at,
           verification_status
    FROM (
        SELECT id,
               json_extract(body, '$.path') as path,
               json_extract(body, '$.size') as size,
               json_extract(body, '$.priority') as priority,
               'pending' as status,
               json_extract(body, '$.retryCount') as retry_count,
               NULL as error_message,
               created as created_at,
               updated as updated_at,
               NULL as completed_at,
               NULL as nzb_path,
               json_extract(body, '$.path') as file_name,
               NULL as script_status,
               0 as script_retry_count,
               NULL as script_last_error,
               NULL as script_next_retry_at,
               NULL as verification_status
        FROM goqite
        WHERE queue = 'file_jobs'

        UNION ALL

        SELECT id,
               path as path,
               size,
               priority,
               'running' as status,
               json_extract(job_data, '$.retryCount') as retry_count,
               NULL as error_message,
               created_at,
               started_at as updated_at,
               NULL as completed_at,
               NULL as nzb_path,
               path as file_name,
               NULL as script_status,
               0 as script_retry_count,
               NULL as script_last_error,
               NULL as script_next_retry_at,
               NULL as verification_status
        FROM in_progress_items

        UNION ALL

        SELECT id, path, size, priority, 'complete' as status,
               0 as retry_count, NULL as error_message,
               created_at, completed_at as updated_at, completed_at, nzb_path,
               path as file_name,
               script_status, script_retry_count, script_last_error, script_next_retry_at,
               verification_status
        FROM completed_items

        UNION ALL

        SELECT id, path, size, priority, 'error' as status,
               json_extract(job_data, '$.retryCount') as retry_count,
               error_message, created_at, errored_at as updated_at,
               NULL as completed_at, NULL as nzb_path,
               path as file_name,
               NULL as script_status,
               0 as script_retry_count,
               NULL as script_last_error,
               NULL as script_next_retry_at,
               NULL as verification_status
        FROM errored_items
    )";

const DURABLE_PROGRESS_SQL: &str = "
    SELECT tf.completed_item_id,
           SUM(tf.article_count) AS total,
           SUM(CASE
                 WHEN tf.verification_state = 'verified' THEN tf.article_count
                 WHEN tf.verification_state IN ('verifying', 'verification_failed')
                   THEN MAX(tf.article_count - COALESCE(vf.open_count, 0), 0)
                 ELSE 0
               END) AS verified
    FROM transfer_files tf
    LEFT JOIN (
        SELECT transfer_id, file_id, COUNT(*) AS open_count
        FROM verification_failures
        WHERE state IN ('pending', 'failed')
        GROUP BY transfer_id, file_id
    ) vf ON vf.transfer_id = tf.transfer_id AND vf.file_id = tf.file_id
    WHERE tf.completed_item_id IN (%s)
    GROUP BY tf.completed_item_id";

const LEGACY_PROGRESS_SQL: &str = "
    SELECT completed_item_id,
           COUNT(*) AS total,
           SUM(CASE WHEN status = 'verified' THEN 1 ELSE 0 END) AS verified
    FROM pending_article_checks
    WHERE completed_item_id IN (%s)
    GROUP BY completed_item_id";

impl Queue {
    fn count(&self, sql: &str, what: &str) -> Result<i64> {
        self.db
            .lock()
            .query_row(sql, [], |r| r.get(0))
            .map_err(|e| QueueError::Msg(format!("failed to get {what} count: {e}")))
    }

    fn query_items(
        &self,
        sql: &str,
        limit: i64,
        offset: i64,
        map: fn(&Row<'_>) -> Option<QueueItem>,
    ) -> Result<Vec<QueueItem>> {
        let conn = self.db.lock();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit, offset], |r| Ok(map(r)))?;
        Ok(rows.filter_map(std::result::Result::ok).flatten().collect())
    }

    /// A page of items with filtering, sorting and metadata.
    pub fn get_queue_items(&self, mut p: PaginationParams) -> Result<PaginatedResult> {
        if p.page < 1 {
            p.page = 1;
        }
        if !(1..=100).contains(&p.limit) {
            p.limit = 25;
        }
        if p.sort_by.is_empty() {
            p.sort_by = "created".into();
        }
        if p.order.is_empty() {
            p.order = "desc".into();
        }
        let offset = (p.page - 1) * p.limit;
        let order_by = build_order_by(&p.sort_by, &p.order);

        let (total, mut items) = match p.status.as_str() {
            "pending" => {
                let n = self.count(
                    "SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'",
                    "pending items",
                )?;
                (
                    n,
                    if n > 0 {
                        self.pending_page(&order_by, offset, p.limit)?
                    } else {
                        vec![]
                    },
                )
            }
            "running" => {
                let n = self.count(
                    "SELECT COUNT(*) FROM in_progress_items",
                    "in-progress items",
                )?;
                (
                    n,
                    if n > 0 {
                        self.in_progress_page(&order_by, offset, p.limit)?
                    } else {
                        vec![]
                    },
                )
            }
            "complete" => {
                let n = self.count("SELECT COUNT(*) FROM completed_items", "completed items")?;
                (
                    n,
                    if n > 0 {
                        self.completed_page(&order_by, offset, p.limit)?
                    } else {
                        vec![]
                    },
                )
            }
            "error" => {
                let n = self.count("SELECT COUNT(*) FROM errored_items", "errored items")?;
                (
                    n,
                    if n > 0 {
                        self.errored_page(&order_by, offset, p.limit)?
                    } else {
                        vec![]
                    },
                )
            }
            _ => {
                let n: i64 = ["pending", "running", "complete", "error"]
                    .iter()
                    .map(|s| self.count_status(s))
                    .sum::<Result<i64>>()?;
                let items = match (n > 0, p.sort_by.as_str()) {
                    (false, _) => vec![],
                    (true, "status") => self.merged_by_status(&p.order, offset, p.limit)?,
                    (true, _) => self.query_items(
                        &format!("{MERGED_SQL} ORDER BY {order_by} LIMIT ?1 OFFSET ?2"),
                        p.limit,
                        offset,
                        merged_item,
                    )?,
                };
                (n, items)
            }
        };
        if total == 0 {
            return Ok(PaginatedResult {
                items: vec![],
                total_items: 0,
                total_pages: 0,
                current_page: p.page,
                items_per_page: p.limit,
                has_next: false,
                has_prev: false,
            });
        }
        self.attach_verification_progress(&mut items);
        let total_pages = (total + p.limit - 1) / p.limit;
        Ok(PaginatedResult {
            items,
            total_items: total,
            total_pages,
            current_page: p.page,
            items_per_page: p.limit,
            has_next: p.page < total_pages,
            has_prev: p.page > 1,
        })
    }

    fn count_status(&self, status: &str) -> Result<i64> {
        let sql = match status {
            "running" => "SELECT COUNT(*) FROM in_progress_items",
            "pending" => "SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'",
            "complete" => "SELECT COUNT(*) FROM completed_items",
            "error" => "SELECT COUNT(*) FROM errored_items",
            other => return msg(format!("unknown status {other:?}")),
        };
        self.count(sql, &format!("{status} items")).map_err(|e| {
            QueueError::Msg(
                e.to_string()
                    .replace("get ", "count ")
                    .replace(" count", ""),
            )
        })
    }

    /// Buckets in status priority order. The offset is consumed against each bucket's total size, not
    /// the page it returns, so a bucket smaller than the offset cannot skip items at the boundary.
    fn merged_by_status(&self, order: &str, mut offset: i64, limit: i64) -> Result<Vec<QueueItem>> {
        let buckets: [&str; 4] = if order == "asc" {
            ["complete", "error", "pending", "running"]
        } else {
            ["running", "pending", "error", "complete"]
        };
        let mut out = Vec::new();
        let mut remaining = limit;
        for status in buckets {
            if remaining <= 0 {
                break;
            }
            let count = self.count_status(status)?;
            if offset >= count {
                offset -= count;
                continue;
            }
            let items = match status {
                "running" => self.in_progress_page("started_at DESC", offset, remaining)?,
                "pending" => self.query_items("SELECT id, created, updated, body FROM goqite WHERE queue = 'file_jobs' ORDER BY priority DESC, created ASC LIMIT ?1 OFFSET ?2", remaining, offset, pending_item)?,
                "complete" => self.completed_page("completed_at DESC", offset, remaining)?,
                _ => self.errored_page("errored_at DESC", offset, remaining)?,
            };
            offset = 0;
            remaining -= items.len() as i64;
            out.extend(items);
        }
        Ok(out)
    }

    fn pending_page(&self, order_by: &str, offset: i64, limit: i64) -> Result<Vec<QueueItem>> {
        let sort = match order_by {
            "created_at DESC" => "created DESC",
            "created_at ASC" => "created ASC",
            "size DESC" => "job_size DESC",
            "size ASC" => "job_size ASC",
            "priority DESC" => "priority DESC",
            "priority ASC" => "priority ASC",
            "file_name DESC" => "job_path DESC",
            "file_name ASC" => "job_path ASC",
            _ => "created DESC",
        };
        self.query_items(&format!("SELECT id, created, updated, body FROM goqite WHERE queue = 'file_jobs' ORDER BY {sort} LIMIT ?1 OFFSET ?2"), limit, offset, pending_item)
    }

    fn in_progress_page(&self, order_by: &str, offset: i64, limit: i64) -> Result<Vec<QueueItem>> {
        self.query_items(&format!("SELECT id, path, size, priority, created_at, started_at, job_data FROM in_progress_items ORDER BY {order_by} LIMIT ?1 OFFSET ?2"), limit, offset, running_item)
    }

    fn completed_page(&self, order_by: &str, offset: i64, limit: i64) -> Result<Vec<QueueItem>> {
        let conn = self.db.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COMPLETED_COLS} FROM completed_items ORDER BY {order_by} LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map(params![limit, offset], completed_item)?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
    }

    fn errored_page(&self, order_by: &str, offset: i64, limit: i64) -> Result<Vec<QueueItem>> {
        self.query_items(&format!("SELECT id, path, size, priority, error_message, created_at, errored_at, job_data FROM errored_items ORDER BY {order_by} LIMIT ?1 OFFSET ?2"), limit, offset, errored_item)
    }

    /// Fills verified/total article counts for items awaiting verification: durable transfers first
    /// (article-count weighted), then the legacy per-article checks for items without transfer rows.
    fn attach_verification_progress(&self, items: &mut [QueueItem]) {
        let mut wanted: std::collections::HashMap<String, usize> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.verification_status.as_deref() == Some("pending_verification"))
            .map(|(n, i)| (i.id.clone(), n))
            .collect();
        if wanted.is_empty() {
            return;
        }
        for template in [DURABLE_PROGRESS_SQL, LEGACY_PROGRESS_SQL] {
            for id in self.apply_counts(items, &wanted, template) {
                wanted.remove(&id);
            }
            if wanted.is_empty() {
                return;
            }
        }
    }

    fn apply_counts(
        &self,
        items: &mut [QueueItem],
        wanted: &std::collections::HashMap<String, usize>,
        template: &str,
    ) -> Vec<String> {
        let ids: Vec<&String> = wanted.keys().collect();
        let sql = template.replace("%s", &vec!["?"; ids.len()].join(","));
        let conn = self.db.lock();
        let run = || -> rusqlite::Result<Vec<(String, i64, i64)>> {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(ids.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect()
        };
        let mut covered = Vec::new();
        match run() {
            Ok(rows) => {
                for (id, total, verified) in rows {
                    if let Some(&i) = wanted.get(&id)
                        && total > 0
                    {
                        items[i].total_articles = Some(total);
                        items[i].verified_articles = Some(verified);
                        covered.push(id);
                    }
                }
            }
            Err(e) => eprintln!("queue: failed to query verification progress: {e}"),
        }
        covered
    }
}
