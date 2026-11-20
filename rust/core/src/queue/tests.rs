use std::path::PathBuf;

use rusqlite::{Connection, params};
use serde_json::Value;

use super::*;
use crate::database::MigrationRunner;
use crate::transferstore::{self as ts, Store, TransferFile, VerificationFailure};

fn migrated() -> Db {
    let db = Db::in_memory().unwrap();
    db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
    MigrationRunner::new(db.clone()).migrate_up().unwrap();
    db
}

fn queue() -> Queue {
    Queue::new(migrated())
}

fn n(q: &Queue, table: &str) -> i64 {
    q.db()
        .lock()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Opens the Go-written queue database (copied into memory) without recovering in-progress jobs.
fn go_queue() -> Queue {
    let copy = tempdir().join("go-queue.db");
    std::fs::copy(fixture("go-queue.db"), &copy).unwrap();
    let db = Db::new(Connection::open(copy).unwrap());
    // the app migrates at startup; the fixture predates migration 011
    crate::database::MigrationRunner::new(db.clone())
        .migrate_up()
        .unwrap();
    Queue::without_recovery(db)
}

/// Every listing the Go queue produced over the same database (all sort/filter/status combinations,
/// including those where Go's own SQL fails) must come out identically.
#[test]
fn listings_match_what_go_produced_over_the_same_database() {
    let oracle: Value =
        serde_json::from_slice(&std::fs::read(fixture("go-queue.json")).unwrap()).unwrap();
    let q = go_queue();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 18);
    let mut errors = 0;
    for c in cases {
        let params: PaginationParams = serde_json::from_value(c["params"].clone()).unwrap();
        let got = q.get_queue_items(params);
        match (c.get("result"), c.get("error")) {
            (Some(want), _) => assert_eq!(
                serde_json::to_value(got.unwrap()).unwrap(),
                *want,
                "params {}",
                c["params"]
            ),
            (None, Some(_)) => {
                errors += 1;
                assert!(
                    got.is_err(),
                    "Go failed but Rust succeeded for {}",
                    c["params"]
                );
            }
            _ => unreachable!(),
        }
    }
    assert!(
        errors == 2,
        "the invalid sort columns must fail like Go ({errors})"
    );
}

#[test]
fn stats_script_retry_checks_and_pending_size_match_go() {
    let oracle: Value =
        serde_json::from_slice(&std::fs::read(fixture("go-queue.json")).unwrap()).unwrap();
    let q = go_queue();
    assert_eq!(Value::Object(q.get_queue_stats().unwrap()), oracle["stats"]);
    let ids = oracle["ids"].as_array().unwrap();
    let (t, p, f) = q
        .get_pending_check_count_for_item(ids[0].as_str().unwrap())
        .unwrap();
    assert_eq!(serde_json::json!([t, p, f]), oracle["checks"]);
    assert_eq!(
        q.pending_total_size().unwrap(),
        oracle["pendingTotal"].as_i64().unwrap()
    );
    let retry = q.get_items_for_script_retry(10).unwrap();
    let want = oracle["retry"].as_array().unwrap();
    assert_eq!(
        (
            retry.len(),
            retry[0].id.as_str(),
            retry[0].script_retry_count
        ),
        (want.len(), want[0]["id"].as_str().unwrap(), 2)
    );
    assert_eq!(retry[0].nzb_path, want[0]["nzbPath"].as_str().unwrap());
    assert_eq!(
        fmt_time(retry[0].script_first_failure_at.unwrap()),
        want[0]["scriptFirstFailureAt"].as_str().unwrap()
    );
}

#[test]
fn a_job_serialises_like_gos_file_job() {
    let j = FileJob {
        path: "/a".into(),
        size: 5,
        priority: 1,
        created_at: parse_time("2026-01-02T03:04:05.5Z").unwrap(),
        retry_count: 2,
        transfer_id: String::new(),
        input_folder: String::new(),
        delete_original: None,
    };
    assert_eq!(
        serde_json::to_string(&j).unwrap(),
        r#"{"path":"/a","size":5,"priority":1,"createdAt":"2026-01-02T03:04:05.5Z","retryCount":2}"#
    );
    let with = FileJob {
        transfer_id: "t".into(),
        input_folder: "/in".into(),
        delete_original: Some(false),
        ..j
    };
    let back: FileJob = serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    assert_eq!(back, with);
    // a body written by Go (extra fields tolerated, missing ones defaulted)
    let go: FileJob = serde_json::from_str(
        r#"{"path":"/x","size":1,"createdAt":"2026-01-02T03:04:05.123456789Z","unknown":1}"#,
    )
    .unwrap();
    assert_eq!(
        (go.priority, go.retry_count, go.path.as_str()),
        (0, 0, "/x")
    );
}

#[test]
fn add_deduplicates_across_every_state_but_the_unchecked_variants_do_not() {
    let q = queue();
    q.add_file("/a", 10).unwrap();
    q.add_file("/a", 10).unwrap();
    q.add_file_with_priority("/a", 10, 5).unwrap();
    q.add_file_with_options("/a", 10, AddOptions::default())
        .unwrap();
    assert_eq!(n(&q, "goqite"), 1);
    q.add_file_without_duplicate_check("/a", 10).unwrap();
    q.add_file_with_priority_without_duplicate_check("/a", 10, 1)
        .unwrap();
    assert_eq!(n(&q, "goqite"), 3);
    // in-progress, completed and errored paths count as tracked
    let r = q.receive_file().unwrap().unwrap();
    q.complete_file(&r.msg_id, "/n.nzb", &r.job).unwrap();
    assert!(q.is_path_in_queue("/a").unwrap());
    assert!(!q.is_path_in_queue("/zzz").unwrap());
    q.add_file("/b", 1).unwrap();
    let rb = q.receive_file().unwrap().unwrap();
    assert!(q.is_path_in_queue("/b").unwrap(), "in progress");
    q.mark_as_error(&rb.msg_id, &rb.job, "x").unwrap();
    assert!(q.is_path_in_queue("/b").unwrap(), "errored");
}

#[test]
fn manual_files_pin_delete_original_false_and_options_travel_with_the_job() {
    let q = queue();
    q.add_manual_file("/m", 1).unwrap();
    q.add_file_with_options(
        "/o",
        2,
        AddOptions {
            priority: 9,
            input_folder: "/in".into(),
            delete_original: Some(true),
        },
    )
    .unwrap();
    let first = q.receive_file().unwrap().unwrap();
    assert_eq!(
        (
            first.job.path.as_str(),
            first.job.priority,
            first.job.input_folder.as_str(),
            first.job.delete_original
        ),
        ("/o", 9, "/in", Some(true)),
        "higher priority first"
    );
    assert!(!first.job.transfer_id.is_empty());
    let second = q.receive_file().unwrap().unwrap();
    assert_eq!(second.job.delete_original, Some(false));
}

#[test]
fn receive_tracks_the_job_and_complete_records_the_verification_kind() {
    let q = queue();
    q.add_file("/a", 100).unwrap();
    q.add_file("/b", 200).unwrap();
    assert_eq!(q.pending_total_size().unwrap(), 300);
    let a = q.receive_file().unwrap().unwrap();
    assert_eq!((n(&q, "in_progress_items"), n(&q, "goqite")), (1, 1));
    q.complete_file(&a.msg_id, "/a.nzb", &a.job).unwrap();
    assert_eq!(
        (n(&q, "in_progress_items"), n(&q, "completed_items")),
        (0, 1)
    );
    let b = q.receive_file().unwrap().unwrap();
    q.complete_reused_file(&b.msg_id, "/b.nzb", &b.job).unwrap();
    let st: Vec<String> = {
        let c = q.db().lock();
        let mut s = c
            .prepare(
                "SELECT verification_status FROM completed_items ORDER BY verification_status DESC",
            )
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(st, ["verified", "reused"], "one of each kind");
    assert!(q.receive_file().unwrap().is_none());
    assert_eq!(q.get_completed_item_nzb_path(&a.msg_id).unwrap(), "/a.nzb");
    assert!(
        q.get_completed_item_nzb_path("nope")
            .unwrap_err()
            .to_string()
            .contains("completed item not found")
    );
}

#[test]
fn an_invalid_body_is_dropped_with_an_error() {
    let q = queue();
    q.db().lock().execute("INSERT INTO goqite (queue, body, timeout) VALUES ('file_jobs', x'7b', '2000-01-01T00:00:00.000Z')", []).unwrap();
    assert!(
        q.receive_file()
            .unwrap_err()
            .to_string()
            .contains("failed to unmarshal job")
    );
    assert_eq!(n(&q, "goqite"), 0);
}

#[test]
fn crash_recovery_requeues_in_progress_jobs_once() {
    let db = migrated();
    let q = Queue::new(db.clone());
    q.add_file("/a", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    assert_eq!(n(&q, "in_progress_items"), 1);
    drop(r);
    let reopened = Queue::new(db); // "crash": reopen
    assert_eq!(
        (n(&reopened, "in_progress_items"), n(&reopened, "goqite")),
        (0, 1)
    );
    let again = reopened.receive_file().unwrap().unwrap();
    assert_eq!(again.job.path, "/a");
}

#[test]
fn error_and_retry_reset_the_retry_count_and_requeue() {
    let q = queue();
    q.add_file("/a", 1).unwrap();
    let mut r = q.receive_file().unwrap().unwrap();
    r.job.retry_count = 4;
    q.mark_as_error(&r.msg_id, &r.job, "boom").unwrap();
    assert_eq!((n(&q, "errored_items"), n(&q, "in_progress_items")), (1, 0));
    q.retry_errored_job(&r.msg_id).unwrap();
    assert_eq!((n(&q, "errored_items"), n(&q, "goqite")), (0, 1));
    assert_eq!(q.receive_file().unwrap().unwrap().job.retry_count, 0);
    assert!(
        q.retry_errored_job("missing")
            .unwrap_err()
            .to_string()
            .contains("errored item not found")
    );
}

#[test]
fn clear_in_progress_prevents_a_leaked_duplicate_on_retry() {
    let q = queue();
    q.add_file("/a", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    q.clear_in_progress(&r.msg_id).unwrap();
    q.readd_job(&r.job).unwrap();
    assert_eq!((n(&q, "in_progress_items"), n(&q, "goqite")), (0, 1));
    q.cancel_file("nothing").unwrap();
}

#[test]
fn priority_updates_change_the_body_and_optionally_the_column() {
    let q = queue();
    q.add_file("/lo", 1).unwrap();
    q.add_file("/hi", 1).unwrap();
    let id: String = q
        .db()
        .lock()
        .query_row(
            "SELECT id FROM goqite WHERE json_extract(body,'$.path')='/hi'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    q.set_queue_item_priority(&id, 7).unwrap();
    let (col, body): (i64, i64) = q
        .db()
        .lock()
        .query_row(
            "SELECT priority, json_extract(body,'$.priority') FROM goqite WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((col, body), (0, 7), "body only");
    q.set_queue_item_priority_with_reorder(&id, 9).unwrap();
    assert_eq!(q.receive_file().unwrap().unwrap().job.path, "/hi");
    assert!(
        q.set_queue_item_priority_with_reorder("nope", 1)
            .unwrap_err()
            .to_string()
            .contains("pending queue item not found")
    );
}

fn durable_item(q: &Queue, id: &str, dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let nzb = dir.join(format!("{id}.nzb"));
    std::fs::write(&nzb, b"<nzb/>").unwrap();
    let job = FileJob {
        path: format!("/src/{id}"),
        size: 1,
        priority: 0,
        created_at: Utc::now(),
        retry_count: 0,
        transfer_id: format!("t-{id}"),
        input_folder: String::new(),
        delete_original: None,
    };
    q.complete_file(id, nzb.to_str().unwrap(), &job).unwrap();
    let manifest = dir.join(format!("t-{id}")).join("f.jsonl.zst");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(&manifest, b"x").unwrap();
    let store = Store::new(q.db().clone());
    store
        .upsert_file(&TransferFile {
            transfer_id: format!("t-{id}"),
            file_id: "f".into(),
            manifest_path: manifest.to_string_lossy().into_owned(),
            source_path: "/s".into(),
            completed_item_id: id.into(),
            ..Default::default()
        })
        .unwrap();
    store
        .add_failure(&VerificationFailure {
            transfer_id: format!("t-{id}"),
            file_id: "f".into(),
            article_index: 0,
            message_id: "<m>".into(),
            ..Default::default()
        })
        .unwrap();
    (nzb, manifest)
}

#[test]
fn removing_a_completed_item_detaches_its_transfer_and_deletes_its_files() {
    let dir = tempdir();
    let q = queue();
    let (nzb, manifest) = durable_item(&q, "ci1", &dir);
    let (nzb2, _) = durable_item(&q, "ci2", &dir);
    q.add_pending_article_checks(
        "ci1",
        &[PendingArticleCheck {
            id: 0,
            completed_item_id: String::new(),
            message_id: "<x>".into(),
            groups: "[]".into(),
            status: String::new(),
            retry_count: 0,
            next_retry_at: Utc::now(),
            first_failure_at: Utc::now(),
            last_checked_at: None,
            created_at: Utc::now(),
        }],
    )
    .unwrap();
    q.remove_from_queue("ci1").unwrap();
    assert!(!nzb.exists() && !manifest.exists() && nzb2.exists());
    assert_eq!(
        (
            n(&q, "completed_items"),
            n(&q, "transfer_files"),
            n(&q, "verification_failures"),
            n(&q, "pending_article_checks")
        ),
        (1, 1, 1, 0)
    );
    assert!(
        q.remove_completed_item("ci1")
            .unwrap_err()
            .to_string()
            .contains("completed item not found")
    );
}

#[test]
fn clear_queue_and_clear_completed_differ_in_what_they_keep() {
    let dir = tempdir();
    let q = queue();
    durable_item(&q, "c1", &dir);
    let (nzb2, _) = durable_item(&q, "c2", &dir);
    q.add_file("/pending", 1).unwrap();
    q.add_file("/err", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    q.mark_as_error(&r.msg_id, &r.job, "e").unwrap();
    q.clear_completed_items().unwrap();
    assert_eq!(
        (
            n(&q, "completed_items"),
            n(&q, "errored_items"),
            n(&q, "goqite"),
            n(&q, "transfer_files")
        ),
        (0, 1, 1, 0)
    );
    assert!(!nzb2.exists());
    q.clear_queue().unwrap();
    assert_eq!((n(&q, "errored_items"), n(&q, "goqite")), (0, 0));
}

#[test]
fn remove_from_queue_handles_every_bucket() {
    let q = queue();
    q.add_file("/p", 1).unwrap();
    q.add_file("/r", 1).unwrap();
    q.add_file("/e", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap(); // highest-created first among equal priority = /p
    let e = q.receive_file().unwrap().unwrap();
    q.mark_as_error(&e.msg_id, &e.job, "x").unwrap();
    q.remove_from_queue(&e.msg_id).unwrap();
    q.remove_from_queue(&r.msg_id).unwrap();
    assert_eq!(
        (
            n(&q, "errored_items"),
            n(&q, "in_progress_items"),
            n(&q, "goqite")
        ),
        (0, 0, 1)
    );
    let left: String = q
        .db()
        .lock()
        .query_row("SELECT id FROM goqite", [], |r| r.get(0))
        .unwrap();
    q.remove_from_queue(&left).unwrap();
    q.remove_from_queue("does-not-exist").unwrap();
    assert_eq!(n(&q, "goqite"), 0);
}

#[test]
fn script_status_lifecycle() {
    let q = queue();
    q.add_file("/a", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    q.complete_file(&r.msg_id, "/a.nzb", &r.job).unwrap();
    let first = parse_time("2026-03-04T05:00:00Z").unwrap();
    q.update_script_status(
        &r.msg_id,
        "pending_retry",
        1,
        "oops",
        Some(Utc::now() - Duration::minutes(1)),
        Some(first),
    )
    .unwrap();
    q.update_script_status(
        &r.msg_id,
        "pending_retry",
        2,
        "oops again",
        Some(Utc::now() - Duration::minutes(1)),
        Some(Utc::now()),
    )
    .unwrap();
    let due = q.get_items_for_script_retry(5).unwrap();
    assert_eq!(
        (due.len(), due[0].script_first_failure_at),
        (1, Some(first)),
        "first failure is kept"
    );
    q.update_script_status(
        &r.msg_id,
        "pending_retry",
        3,
        "later",
        Some(Utc::now() + Duration::hours(1)),
        None,
    )
    .unwrap();
    assert!(
        q.get_items_for_script_retry(5).unwrap().is_empty(),
        "not due yet"
    );
    q.mark_script_failed(&r.msg_id, "gave up").unwrap();
    let (st, err): (String, String) = q
        .db()
        .lock()
        .query_row(
            "SELECT script_status, script_last_error FROM completed_items",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((st.as_str(), err.as_str()), ("failed_permanent", "gave up"));
    q.mark_script_completed(&r.msg_id).unwrap();
    let (st, err, first): (String, Option<String>, Option<String>) = q
        .db()
        .lock()
        .query_row(
            "SELECT script_status, script_last_error, script_first_failure_at FROM completed_items",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((st.as_str(), err, first), ("completed", None, None));
}

#[test]
fn legacy_article_checks_flow() {
    let q = queue();
    q.add_file("/a", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    q.complete_file(&r.msg_id, "/a.nzb", &r.job).unwrap();
    q.update_completed_item_verification_status(&r.msg_id, "pending_verification")
        .unwrap();
    let mk = |m: &str, due: DateTime<Utc>| PendingArticleCheck {
        id: 0,
        completed_item_id: String::new(),
        message_id: m.into(),
        groups: "[\"alt.x\"]".into(),
        status: String::new(),
        retry_count: 0,
        next_retry_at: due,
        first_failure_at: Utc::now(),
        last_checked_at: None,
        created_at: Utc::now(),
    };
    q.add_pending_article_checks(
        &r.msg_id,
        &[
            mk("<1>", Utc::now() - Duration::minutes(2)),
            mk("<2>", Utc::now() - Duration::minutes(1)),
            mk("<3>", Utc::now() + Duration::hours(1)),
        ],
    )
    .unwrap();
    let due = q.get_articles_for_check(10).unwrap();
    assert_eq!(
        due.iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        ["<1>", "<2>"],
        "oldest due first, future excluded"
    );
    assert_eq!(due[0].groups, "[\"alt.x\"]");
    q.mark_article_verified(due[0].id).unwrap();
    q.update_article_check_retry(due[1].id, 3, Utc::now() + Duration::hours(1))
        .unwrap();
    assert!(q.get_articles_for_check(10).unwrap().is_empty());
    assert_eq!(
        q.get_pending_check_count_for_item(&r.msg_id).unwrap(),
        (3, 2, 0)
    );
    q.mark_article_check_failed(due[1].id).unwrap();
    assert_eq!(
        q.get_pending_check_count_for_item(&r.msg_id).unwrap(),
        (3, 1, 1)
    );
    assert_eq!(
        q.get_pending_check_count_for_item("none").unwrap(),
        (0, 0, 0)
    );
    let page = q
        .get_queue_items(PaginationParams {
            status: "complete".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        (
            page.items[0].verified_articles,
            page.items[0].total_articles
        ),
        (Some(1), Some(3)),
        "legacy progress"
    );
}

#[test]
fn durable_progress_weighs_files_by_article_count_and_open_failures() {
    let q = queue();
    q.add_file("/a", 1).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    q.complete_file(&r.msg_id, "/a.nzb", &r.job).unwrap();
    q.update_completed_item_verification_status(&r.msg_id, "pending_verification")
        .unwrap();
    let store = Store::new(q.db().clone());
    for (f, cnt, state) in [
        ("done", 10, ts::STATE_VERIFIED),
        ("mid", 20, ts::STATE_VERIFYING),
        ("new", 5, ts::STATE_UPLOADED),
    ] {
        store
            .upsert_file(&TransferFile {
                transfer_id: "t".into(),
                file_id: f.into(),
                manifest_path: "/m".into(),
                source_path: "/s".into(),
                completed_item_id: r.msg_id.clone(),
                article_count: cnt,
                ..Default::default()
            })
            .unwrap();
        store
            .set_verification_state("t", f, state, None, "")
            .unwrap();
    }
    for i in 0..4 {
        store
            .add_failure(&VerificationFailure {
                transfer_id: "t".into(),
                file_id: "mid".into(),
                article_index: i,
                message_id: format!("<{i}>"),
                ..Default::default()
            })
            .unwrap();
    }
    let page = q.get_queue_items(PaginationParams::default()).unwrap();
    assert_eq!(
        (
            page.items[0].verified_articles,
            page.items[0].total_articles
        ),
        (Some(10 + 16), Some(35))
    );
}

#[test]
fn stats_listing_and_status_buckets_without_a_database_file() {
    let q = queue();
    assert_eq!(
        q.get_queue_items(PaginationParams::default())
            .unwrap()
            .total_items,
        0
    );
    for i in 0..5 {
        q.add_file(&format!("/f{i}"), i + 1).unwrap();
    }
    let page = q
        .get_queue_items(PaginationParams {
            limit: 2,
            page: 3,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        (
            page.items.len(),
            page.total_pages,
            page.has_next,
            page.has_prev
        ),
        (1, 3, false, true)
    );
    let s = q.get_queue_stats().unwrap();
    assert_eq!(
        (
            s["pending"].as_i64(),
            s["total"].as_i64(),
            s["total_including_completed"].as_i64()
        ),
        (Some(5), Some(5), Some(5))
    );
    let d = q
        .debug_queue_item(
            &q.get_queue_items(PaginationParams::default())
                .unwrap()
                .items[0]
                .id,
        )
        .unwrap();
    assert_eq!(d["received"], 0);
    assert!(
        q.debug_queue_item("nope")
            .unwrap_err()
            .to_string()
            .contains("job not found")
    );
}

fn tempdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "queue-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A real upload-copy file (`<temp>/postie-<n>/a.bin`) so the discard logic recognises it.
fn upload_copy() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("postie-{}", rand::random::<u32>()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.bin");
    std::fs::write(&f, b"x").unwrap();
    (dir, f)
}

fn pending_id(q: &Queue) -> String {
    q.db()
        .lock()
        .query_row("SELECT id FROM goqite WHERE queue = 'file_jobs'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn a_path_that_is_running_is_never_discarded_by_remove_or_clear() {
    let q = queue();
    let (dir, f) = upload_copy();
    let p = f.to_string_lossy().into_owned();
    q.add_file(&p, 1).unwrap();
    let _running = q.receive_file().unwrap().unwrap();
    // the window in receive_file: the job is both running and still pending
    q.add_file_without_duplicate_check(&p, 1).unwrap();
    q.remove_from_queue(&pending_id(&q)).unwrap();
    assert!(
        f.exists(),
        "remove_from_queue must not delete a running job's file"
    );
    q.add_file_without_duplicate_check(&p, 1).unwrap();
    q.clear_queue().unwrap();
    assert!(
        f.exists(),
        "clear_queue must not delete a running job's file"
    );
    assert_eq!(n(&q, "in_progress_items"), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn removing_pending_or_errored_upload_copies_deletes_them_but_not_user_files() {
    let q = queue();
    let (dir, f) = upload_copy();
    let p = f.to_string_lossy().into_owned();
    q.add_file(&p, 1).unwrap();
    q.remove_from_queue(&pending_id(&q)).unwrap();
    assert!(
        !f.exists() && !dir.exists(),
        "copy and its empty upload dir go"
    );

    let user = tempdir().join("keep.mkv");
    std::fs::write(&user, b"x").unwrap();
    q.add_file(&user.to_string_lossy(), 1).unwrap();
    q.remove_from_queue(&pending_id(&q)).unwrap();
    q.add_file(&user.to_string_lossy(), 1).unwrap();
    q.clear_queue().unwrap();
    assert!(user.exists(), "non-upload files are never deleted");
    std::fs::remove_dir_all(user.parent().unwrap()).unwrap();
}

#[test]
fn removing_a_completed_upload_copy_discards_it_unless_another_row_uses_it() {
    let q = queue();
    for mode in ["remove", "clear_completed", "clear_queue"] {
        let (dir, f) = upload_copy();
        let p = f.to_string_lossy().into_owned();
        q.add_file(&p, 1).unwrap();
        let r = q.receive_file().unwrap().unwrap();
        q.complete_file(&r.msg_id, "/x.nzb", &r.job).unwrap();
        // another live row still references the path: keep the copy
        q.add_file_without_duplicate_check(&p, 1).unwrap();
        match mode {
            "remove" => q.remove_from_queue(&r.msg_id).unwrap(),
            "clear_completed" => q.clear_completed_items().unwrap(),
            _ => {}
        }
        if mode != "clear_queue" {
            assert!(f.exists(), "{mode}: still referenced by a pending row");
            q.remove_from_queue(&pending_id(&q)).unwrap();
            assert!(!f.exists() && !dir.exists(), "{mode}: now unreferenced");
        } else {
            q.clear_queue().unwrap();
            assert!(!f.exists() && !dir.exists(), "{mode}: everything cleared");
        }
    }
}

/// A file the user adds again uploads again once its earlier run finished or failed; only a copy
/// that is still waiting or uploading is skipped. (Desktop adds files in place, so re-adding the
/// same path used to do nothing while reporting success.)
#[test]
fn a_user_re_add_requeues_finished_or_failed_paths_but_not_active_ones() {
    let q = queue();
    q.add_user_file("/a", 10, AddOptions::default()).unwrap();
    assert!(
        !q.add_user_file("/a", 10, AddOptions::default()).unwrap(),
        "still pending"
    );
    assert_eq!(n(&q, "goqite"), 1);
    let r = q.receive_file().unwrap().unwrap();
    assert!(
        !q.add_user_file("/a", 10, AddOptions::default()).unwrap(),
        "uploading"
    );
    q.complete_file(&r.msg_id, "/a.nzb", &r.job).unwrap();
    assert!(
        q.add_user_file("/a", 10, AddOptions::default()).unwrap(),
        "finished: queued again"
    );
    assert_eq!(n(&q, "goqite"), 1);
    let r = q.receive_file().unwrap().unwrap();
    q.mark_as_error(&r.msg_id, &r.job, "x").unwrap();
    assert!(
        q.add_user_file("/a", 10, AddOptions::default()).unwrap(),
        "failed: queued again"
    );
    assert_eq!(n(&q, "goqite"), 1);
    // the watcher path keeps skipping anything it has seen
    q.add_file("/a", 10).unwrap();
    assert_eq!(n(&q, "goqite"), 1);
}
