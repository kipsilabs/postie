//! Port of `Reposter` and `articleFromRecord`: re-posts individual articles during durable
//! verification, reading each body from its source file and posting through the shared upload client
//! and engine so re-posts share the process-wide resource limits.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    ArticleClient, PostError, Stats, StatsSnapshot, new_file_read_slots, post_yenc,
    read_at_with_stall_guard, validate_read_range,
};
use crate::article::Article;
use crate::engine::Engine;
use crate::manifest::ArticleRecord;
use crate::throttle::Throttle;

/// Rebuilds an article from a manifest record so it can be re-posted byte for byte with its original
/// Message-ID and headers.
pub fn article_from_record(rec: &ArticleRecord) -> Article {
    Article {
        message_id: rec.message_id.clone(),
        subject: rec.subject.clone(),
        original_subject: rec.original_subject.clone(),
        from: rec.from.clone(),
        groups: rec.groups.clone(),
        part_number: rec.part_number,
        total_parts: rec.total_parts,
        file_name: rec.file_name.clone(),
        date: rec.date,
        file_number: 0,
        offset: rec.offset,
        size: rec.body_size,
        file_size: rec.file_size,
        original_name: String::new(),
        custom_headers: rec.custom_headers.clone(),
        xnxg_header: rec.xnxg_header.clone(),
        hash: String::new(),
    }
}

pub struct Reposter {
    upload: Option<Arc<dyn ArticleClient>>,
    engine: Arc<Engine>,
    throttle: Option<Arc<Throttle>>,
    stats: Stats,
}

impl Reposter {
    /// If `throttle_rate > 0` the engine-wide throttle (shared with uploads) is applied, so re-posts
    /// and uploads together stay within the configured rate.
    pub fn new(
        upload: Option<Arc<dyn ArticleClient>>,
        engine: Arc<Engine>,
        throttle_rate: i64,
    ) -> Self {
        let throttle = engine.shared_throttle(throttle_rate);
        Self {
            upload,
            engine,
            throttle,
            stats: Stats::new(),
        }
    }

    /// Re-posts one article: the body is read from `rec.source_path` at `rec.offset` and the post reuses
    /// the record's Message-ID and headers so the NZB stays correct.
    pub async fn repost(
        &self,
        cancel: &CancellationToken,
        rec: &ArticleRecord,
    ) -> Result<(), PostError> {
        let upload = self
            .upload
            .as_ref()
            .ok_or_else(|| PostError::Other("reposter has no upload pool".into()))?;
        let file = File::open(Path::new(&rec.source_path)).map_err(|e| {
            let msg = format!("repost: open source {:?}: {e}", rec.source_path);
            if e.kind() == std::io::ErrorKind::NotFound {
                PostError::SourceMissing(msg)
            } else {
                PostError::Other(msg)
            }
        })?;
        let (offset, size) = validate_read_range(&file, rec.offset, rec.body_size)
            .map_err(|e| PostError::Other(format!("repost: {:?}: {e}", rec.source_path)))?;
        let _buffer = self
            .engine
            .reserve_buffer(cancel, self.engine.per_article_bytes())
            .await
            .map_err(|_| PostError::Canceled)?;
        let body = read_at_with_stall_guard(
            &Arc::new(file),
            &new_file_read_slots(),
            offset,
            size,
            cancel,
        )
        .await
        .map_err(|e| {
            PostError::Other(format!("repost: read body at offset {}: {e}", rec.offset))
        })?;
        let _worker = self
            .engine
            .acquire_worker(cancel)
            .await
            .map_err(|_| PostError::Canceled)?;
        post_yenc(
            cancel,
            upload.as_ref(),
            self.throttle.as_deref(),
            Some(&self.stats),
            Some(self.engine.upload_meter()),
            &article_from_record(rec),
            &body,
        )
        .await
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }
}
