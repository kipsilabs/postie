//! Port of `internal/transferwriter`: records a durable manifest and a `transfer_files` row for each
//! file of a transfer before its articles are posted, so an interrupted upload can be resumed and
//! verified with the exact Message-IDs and headers that were posted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::article::Article;
use crate::manifest::{self, ArticleRecord, ManifestError, Reader, Writer};
use crate::poster::{BoxFut, ManifestSink};
use crate::transferstore::{self as ts, Store, TransferFile};

/// Writes manifests and rows for one transfer; created per job and sharing the process-wide store.
pub struct Recorder {
    transfer_id: String,
    base_dir: PathBuf,
    store: Store,
    generated: Mutex<HashSet<String>>,
}

/// Stable identifier for a source path (first 8 bytes of its SHA-256, hex), so re-recording the same
/// file after a retry or crash maps to the same manifest and row.
pub fn file_id(source_path: &str) -> String {
    hex::encode(&Sha256::digest(source_path.as_bytes())[..8])
}

impl Recorder {
    pub fn new(transfer_id: &str, base_dir: &Path, store: Store) -> Self {
        Self {
            transfer_id: transfer_id.into(),
            base_dir: base_dir.into(),
            store,
            generated: Mutex::new(HashSet::new()),
        }
    }

    /// Records that Postie itself wrote these paths (PAR2 files). Only such files get the
    /// `generated_par2` role, which lets the cleaner delete them once verified; any other file is an
    /// original the user placed there. Call before the files' articles are recorded.
    pub fn mark_generated<I, S>(&self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.generated
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend(paths.into_iter().map(Into::into));
    }

    fn role_for(&self, source_path: &str) -> &'static str {
        if self
            .generated
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(source_path)
        {
            manifest::ROLE_GENERATED_PAR2
        } else {
            manifest::ROLE_ORIGINAL
        }
    }

    /// Writes an immutable manifest of the articles (temp file + atomic rename) and upserts the
    /// `transfer_files` row in the planned state. Must run before the articles are posted.
    pub fn record_file(&self, source_path: &str, articles: &[Article]) -> Result<(), String> {
        let fid = file_id(source_path);
        let role = self.role_for(source_path);
        let path = manifest::file_path(&self.base_dir, &self.transfer_id, &fid);
        let mut w = Writer::new(&path).map_err(|e| e.to_string())?;
        for (i, a) in articles.iter().enumerate() {
            if let Err(e) = w.write(&ArticleRecord::from_article(i as i64, source_path, role, a)) {
                let _ = w.abort();
                return Err(e.to_string());
            }
        }
        w.commit().map_err(|e| e.to_string())?;
        self.store
            .upsert_file(&TransferFile {
                transfer_id: self.transfer_id.clone(),
                file_id: fid,
                manifest_path: path.to_string_lossy().into_owned(),
                manifest_version: manifest::VERSION,
                source_path: source_path.into(),
                file_role: role.into(),
                article_count: articles.len() as i64,
                upload_state: ts::STATE_PLANNED.into(),
                verification_state: ts::STATE_PLANNED.into(),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
    }

    /// The article records of a previously written manifest, if any. On crash recovery this lets the
    /// poster reuse the exact Message-IDs/offsets already planned (and possibly partly posted) instead of
    /// regenerating them, which would create duplicate NZB segments. `None` for a fresh upload.
    pub fn existing_articles(
        &self,
        source_path: &str,
    ) -> Result<Option<Vec<ArticleRecord>>, String> {
        let path = manifest::file_path(&self.base_dir, &self.transfer_id, &file_id(source_path));
        match std::fs::metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        }
        let mut reader = Reader::open(&path).map_err(|e: ManifestError| e.to_string())?;
        let mut recs = Vec::new();
        while let Some(rec) = reader.next_record().map_err(|e| e.to_string())? {
            recs.push(rec);
        }
        Ok((!recs.is_empty()).then_some(recs))
    }

    /// Marks every file of the transfer uploaded with `posted_at` and the first verification due time,
    /// so the durable verification service can pick them up. With `delete_original`, original files also
    /// get the delete-original cleanup policy persisted (PAR2 cleanup follows `maintain_par2` instead).
    pub fn complete_upload(
        &self,
        posted_at: DateTime<Utc>,
        next_check_at: DateTime<Utc>,
        delete_original: bool,
    ) -> Result<(), String> {
        self.store
            .mark_transfer_uploaded(
                &self.transfer_id,
                posted_at,
                next_check_at,
                delete_original.then_some(ts::CLEANUP_DELETE_ORIGINAL),
                manifest::ROLE_ORIGINAL,
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl ManifestSink for Recorder {
    fn record_file<'a>(
        &'a self,
        file_path: &'a Path,
        articles: &'a [Article],
    ) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            crate::blocking::in_place(|| {
                Recorder::record_file(self, &file_path.to_string_lossy(), articles)
            })
        })
    }

    fn existing_articles<'a>(
        &'a self,
        file_path: &'a Path,
    ) -> BoxFut<'a, Result<Option<Vec<ArticleRecord>>, String>> {
        Box::pin(async move {
            crate::blocking::in_place(|| {
                Recorder::existing_articles(self, &file_path.to_string_lossy())
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn env() -> (Recorder, Store, PathBuf) {
        let db = Db::in_memory().unwrap();
        db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
        crate::database::MigrationRunner::new(db.clone())
            .migrate_up()
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "tw-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(db);
        (Recorder::new("t1", &dir, store.clone()), store, dir)
    }

    fn articles(n: usize) -> Vec<Article> {
        (0..n)
            .map(|i| Article {
                message_id: format!("<m{i}@x>"),
                subject: "s".into(),
                original_subject: "os".into(),
                from: "f <f@x.y>".into(),
                groups: vec!["alt.test".into()],
                part_number: i as i64 + 1,
                total_parts: n as i64,
                file_name: "a.bin".into(),
                date: chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:20Z").unwrap(),
                offset: i as i64 * 100,
                size: 100,
                file_size: n as i64 * 100,
                file_number: 1,
                original_name: String::new(),
                custom_headers: Default::default(),
                xnxg_header: String::new(),
                hash: String::new(),
            })
            .collect()
    }

    #[test]
    fn writes_the_manifest_and_a_planned_row() {
        let (r, store, _) = env();
        r.record_file("/src/a.bin", &articles(3)).unwrap();
        let f = store
            .get_file("t1", &file_id("/src/a.bin"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                f.article_count,
                f.file_role.as_str(),
                f.upload_state.as_str(),
                f.verification_state.as_str(),
                f.manifest_version
            ),
            (3, "original", "planned", "planned", 1)
        );
        let mut rd = Reader::open(Path::new(&f.manifest_path)).unwrap();
        let mut ids = vec![];
        while let Some(rec) = rd.next_record().unwrap() {
            ids.push((rec.index, rec.message_id));
        }
        assert_eq!(
            ids,
            [
                (0, "<m0@x>".to_string()),
                (1, "<m1@x>".into()),
                (2, "<m2@x>".into())
            ]
        );
    }

    #[test]
    fn role_follows_mark_generated_and_ids_are_deterministic() {
        let (r, store, _) = env();
        r.mark_generated(["/tmp/a.par2"]);
        r.record_file("/tmp/a.par2", &articles(1)).unwrap();
        r.record_file("/user/b.par2", &articles(1)).unwrap();
        assert_eq!(
            store
                .get_file("t1", &file_id("/tmp/a.par2"))
                .unwrap()
                .unwrap()
                .file_role,
            "generated_par2"
        );
        assert_eq!(
            store
                .get_file("t1", &file_id("/user/b.par2"))
                .unwrap()
                .unwrap()
                .file_role,
            "original"
        );
        assert_eq!(file_id("/x"), file_id("/x"));
        assert_eq!(file_id("/x").len(), 16);
        r.record_file("/user/b.par2", &articles(5)).unwrap();
        assert_eq!(
            store.list_files_by_transfer("t1").unwrap().len(),
            2,
            "re-recording updates in place"
        );
        assert_eq!(
            store
                .get_file("t1", &file_id("/user/b.par2"))
                .unwrap()
                .unwrap()
                .article_count,
            5
        );
    }

    #[test]
    fn complete_upload_marks_uploaded_and_sets_delete_policy_on_originals_only() {
        let (r, store, _) = env();
        r.mark_generated(["/tmp/a.par2"]);
        r.record_file("/src/a.bin", &articles(1)).unwrap();
        r.record_file("/tmp/a.par2", &articles(1)).unwrap();
        let posted = Utc::now();
        r.complete_upload(posted, posted + chrono::Duration::seconds(30), true)
            .unwrap();
        let a = store
            .get_file("t1", &file_id("/src/a.bin"))
            .unwrap()
            .unwrap();
        let p = store
            .get_file("t1", &file_id("/tmp/a.par2"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                a.upload_state.as_str(),
                a.cleanup_policy.as_str(),
                p.upload_state.as_str(),
                p.cleanup_policy.as_str()
            ),
            ("uploaded", "delete_original", "uploaded", "")
        );
        assert!(a.next_check_at.is_some());
    }

    #[test]
    fn existing_articles_round_trips_and_is_none_for_fresh_uploads() {
        let (r, _, _) = env();
        assert_eq!(r.existing_articles("/src/a.bin").unwrap(), None);
        r.record_file("/src/a.bin", &articles(4)).unwrap();
        let recs = r.existing_articles("/src/a.bin").unwrap().unwrap();
        assert_eq!(
            (recs.len(), recs[3].message_id.as_str(), recs[3].offset),
            (4, "<m3@x>", 300)
        );
    }

    #[tokio::test]
    async fn works_through_the_manifest_sink_trait() {
        let (r, _, _) = env();
        let sink: &dyn ManifestSink = &r;
        sink.record_file(Path::new("/src/a.bin"), &articles(2))
            .await
            .unwrap();
        assert_eq!(
            sink.existing_articles(Path::new("/src/a.bin"))
                .await
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }
}
