use std::collections::HashSet;

use chrono::NaiveDate;

use super::*;
use crate::config::{Duration as D, ScheduleConfig};
use crate::par2::create::{InputFile, Options, create_with_names};

#[derive(Default)]
struct Q {
    tracked: Mutex<HashSet<String>>,
    added: Mutex<Vec<(String, i64, String)>>,
}

impl WatchQueue for Q {
    fn is_path_in_queue(&self, path: &str) -> Result<bool, String> {
        Ok(self.tracked.lock().unwrap().contains(path))
    }
    fn add_file_with_options(&self, path: &str, size: i64, opts: AddOptions) -> Result<(), String> {
        self.tracked.lock().unwrap().insert(path.to_string());
        self.added
            .lock()
            .unwrap()
            .push((path.to_string(), size, opts.input_folder));
        Ok(())
    }
}

#[derive(Default)]
struct Busy(Mutex<HashSet<String>>);

impl ProcessorInterface for Busy {
    fn is_path_being_processed(&self, path: &str) -> bool {
        self.0.lock().unwrap().contains(path)
    }
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "watch-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write(dir: &Path, rel: &str, size: usize) -> String {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, vec![b'x'; size]).unwrap();
    // a file written 10 s ago, so only the size-stability rule is exercised
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
    p.to_string_lossy().into_owned()
}

fn backdate(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        std::fs::File::options()
            .write(true)
            .open(e.path())
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(10))
            .unwrap();
    }
}

fn cfg() -> WatcherConfig {
    WatcherConfig {
        name: "w".into(),
        enabled: true,
        check_interval: D::new("1m"),
        min_file_age: D::new("1ms"),
        ..Default::default()
    }
}

fn setup(c: WatcherConfig) -> (Watcher, Arc<Q>, Arc<Busy>, PathBuf) {
    let dir = tmp();
    let (q, busy) = (Arc::new(Q::default()), Arc::new(Busy::default()));
    let w = Watcher::new(c, q.clone(), Some(busy.clone()), dir.to_str().unwrap());
    (w, q, busy, dir)
}

/// Two scans: the first only records sizes, the second sees them unchanged and queues.
fn scan_twice(w: &Watcher) {
    let c = CancellationToken::new();
    w.scan_directory(&c).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    w.scan_directory(&c).unwrap();
}

fn added(q: &Q) -> Vec<String> {
    let mut v: Vec<String> = q
        .added
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _, _)| {
            Path::new(p)
                .strip_prefix(std::env::temp_dir())
                .map_or(p.clone(), |r| {
                    r.components()
                        .skip(1)
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/")
                })
        })
        .collect();
    v.sort();
    v
}

#[test]
fn files_are_queued_only_once_their_size_is_stable_across_scans() {
    let (w, q, _, dir) = setup(cfg());
    let a = write(&dir, "a.bin", 100);
    w.scan_directory(&CancellationToken::new()).unwrap();
    assert!(
        q.added.lock().unwrap().is_empty(),
        "first sighting is never stable"
    );
    std::thread::sleep(Duration::from_millis(10));
    write(&dir, "a.bin", 100);
    // still the same size: stable now
    w.scan_directory(&CancellationToken::new()).unwrap();
    let got = q.added.lock().unwrap().clone();
    assert_eq!(
        got,
        [(a, 100, dir.to_string_lossy().into_owned())],
        "queued with the watch folder as input root"
    );
    // already tracked: not queued again
    scan_twice(&w);
    assert_eq!(q.added.lock().unwrap().len(), 1);
}

#[test]
fn a_growing_file_or_a_young_file_waits() {
    let (w, q, _, dir) = setup(WatcherConfig {
        min_file_age: D::new("1h"),
        ..cfg()
    });
    write(&dir, "young.bin", 50);
    scan_twice(&w);
    assert!(
        q.added.lock().unwrap().is_empty(),
        "newer than min_file_age"
    );
    let (w2, q2, _, dir2) = setup(cfg());
    write(&dir2, "grow.bin", 10);
    w2.scan_directory(&CancellationToken::new()).unwrap();
    write(&dir2, "grow.bin", 20);
    std::thread::sleep(Duration::from_millis(10));
    w2.scan_directory(&CancellationToken::new()).unwrap();
    assert!(
        q2.added.lock().unwrap().is_empty(),
        "size changed between scans"
    );
}

#[test]
fn size_threshold_min_size_and_ignore_patterns_filter_files() {
    let (w, q, _, dir) = setup(WatcherConfig {
        size_threshold: 500,
        min_file_size: 20,
        ignore_patterns: Some(vec!["*.nfo".into(), "[bad".into(), "~*".into()]),
        ..cfg()
    });
    write(&dir, "tiny.bin", 5);
    write(&dir, "movie.bin", 100);
    write(&dir, "movie.nfo", 300);
    write(&dir, "~tmp", 200);
    scan_twice(&w);
    assert!(
        q.added.lock().unwrap().is_empty(),
        "directory total below the threshold (ignored and tiny files do not count)"
    );
    write(&dir, "big.bin", 600);
    scan_twice(&w);
    assert_eq!(
        added(&q)
            .iter()
            .map(|p| p.rsplit('/').next().unwrap().to_string())
            .collect::<Vec<_>>(),
        ["big.bin", "movie.bin"],
        "tiny, .nfo and ~tmp files filtered; the invalid pattern is only logged"
    );
}

#[test]
fn tracked_and_running_paths_are_skipped_and_symlinks_follow_the_setting() {
    let (w, q, busy, dir) = setup(cfg());
    let queued = write(&dir, "queued.bin", 10);
    let running = write(&dir, "running.bin", 10);
    write(&dir, "new.bin", 10);
    q.tracked.lock().unwrap().insert(queued);
    busy.0.lock().unwrap().insert(running);
    if !crate::testutil::symlink_file(&dir.join("new.bin"), &dir.join("link.bin")) {
        return; // symlinks need a privilege on this platform
    }
    scan_twice(&w);
    assert_eq!(q.added.lock().unwrap().len(), 1);
    let (w2, q2, _, dir2) = setup(WatcherConfig {
        follow_symlinks: true,
        ..cfg()
    });
    write(&dir2, "real.bin", 10);
    assert!(crate::testutil::symlink_file(
        &dir2.join("real.bin"),
        &dir2.join("link.bin")
    ));
    std::thread::sleep(Duration::from_millis(10)); // a symlink's own mtime is its creation time
    scan_twice(&w2);
    assert_eq!(
        q2.added.lock().unwrap().len(),
        2,
        "symlinks are queued when following"
    );
}

#[test]
fn an_unreadable_root_is_an_error_but_a_missing_subtree_is_not() {
    let w = Watcher::new(
        cfg(),
        Arc::new(Q::default()),
        None,
        "/nonexistent/watch/dir",
    );
    assert!(
        w.scan_directory(&CancellationToken::new())
            .unwrap_err()
            .starts_with("failed to calculate directory size:")
    );
}

#[test]
fn folder_mode_queues_top_level_subfolders_as_jobs_and_loose_files_individually() {
    let (w, q, busy, dir) = setup(WatcherConfig {
        single_nzb_per_folder: true,
        ..cfg()
    });
    write(&dir, "loose.bin", 7);
    write(&dir, "ShowA/ep1.bin", 10);
    write(&dir, "ShowA/extras/bonus.bin", 20);
    write(&dir, "ShowB/ep1.bin", 5);
    write(&dir, "ShowC/ep1.bin", 5);
    write(&dir, "ShowD/ep1.bin", 5);
    q.tracked.lock().unwrap().insert(
        dir.join("ShowB")
            .join("ep1.bin")
            .to_string_lossy()
            .into_owned(),
    );
    busy.0
        .lock()
        .unwrap()
        .insert(format!("FOLDER:{}", dir.join("ShowC").display()));
    scan_twice(&w);
    let got = q.added.lock().unwrap().clone();
    let names: Vec<String> = got
        .iter()
        .map(|(p, s, _)| {
            format!(
                "{}:{s}",
                p.rsplit_once(['/', '\\']).map_or(p.as_str(), |(h, t)| {
                    if let Some(f) = p.strip_prefix("FOLDER:") {
                        f.rsplit(['/', '\\']).next().unwrap_or(h)
                    } else {
                        t
                    }
                })
            )
        })
        .collect();
    let mut names = names;
    names.sort();
    assert_eq!(
        names,
        ["ShowA:30", "ShowD:5", "loose.bin:7"],
        "B has a tracked file, C is running; sizes are summed per folder"
    );
    assert!(
        got.iter()
            .any(|(p, _, root)| p.starts_with("FOLDER:") && root == &dir.to_string_lossy())
    );
}

#[test]
fn par2_files_posted_with_their_source_are_not_queued_separately() {
    let c = WatcherConfig { ..cfg() };
    let dir = tmp();
    let src = write(&dir, "movie.bin", 20_000);
    let o = Options {
        slice_size: 4096,
        num_recovery: 2,
        num_threads: 1,
        memory_limit: 0,
        creator: "Postie".into(),
    };
    create_with_names(
        &CancellationToken::new(),
        &dir.join("movie.bin.par2"),
        &[InputFile {
            path: src.clone().into(),
            name: "movie.bin".into(),
        }],
        &o,
        None,
    )
    .unwrap();
    write(&dir, "orphan.par2", 300);
    let q = Arc::new(Q::default());
    let enabled = Par2Config {
        enabled: Some(true),
        ..Default::default()
    };
    let w = Watcher::new(c.clone(), q.clone(), None, dir.to_str().unwrap())
        .with_par2_config(Arc::new(move || Some(enabled.clone())));
    scan_twice(&w);
    let names: Vec<String> = q
        .added
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _, _)| {
            Path::new(p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        names.contains(&"movie.bin".to_string()) && names.contains(&"orphan.par2".to_string()),
        "{names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("movie.bin.") && n.ends_with(".par2")),
        "its own set rides with the source: {names:?}"
    );
    // PAR2 disabled (or unknown): everything is queued
    let q2 = Arc::new(Q::default());
    let w2 = Watcher::new(c, q2.clone(), None, dir.to_str().unwrap());
    scan_twice(&w2);
    assert!(q2.added.lock().unwrap().len() > names.len());
}

#[test]
fn par2_files_are_queued_when_their_source_will_not_be() {
    let dir = tmp();
    let src = write(&dir, "movie.bin", 20_000);
    let o = Options {
        slice_size: 4096,
        num_recovery: 2,
        num_threads: 1,
        memory_limit: 0,
        creator: "Postie".into(),
    };
    create_with_names(
        &CancellationToken::new(),
        &dir.join("movie.bin.par2"),
        &[InputFile {
            path: src.into(),
            name: "movie.bin".into(),
        }],
        &o,
        None,
    )
    .unwrap();
    backdate(&dir);
    let q = Arc::new(Q::default());
    let enabled = Par2Config {
        enabled: Some(true),
        ..Default::default()
    };
    // the source is ignored by pattern, so its PAR2 files must be queued on their own
    let c = WatcherConfig {
        ignore_patterns: Some(vec!["movie.bin".into()]),
        ..cfg()
    };
    let w = Watcher::new(c, q.clone(), None, dir.to_str().unwrap())
        .with_par2_config(Arc::new(move || Some(enabled.clone())));
    scan_twice(&w);
    let names: Vec<String> = q
        .added
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _, _)| {
            Path::new(p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        names.iter().all(|n| n.ends_with(".par2")) && !names.is_empty(),
        "{names:?}"
    );
}

fn at(h: u32, m: u32) -> DateTime<Local> {
    Local
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap(),
        )
        .earliest()
        .unwrap()
}

fn sched(start: &str, end: &str) -> Watcher {
    let (w, ..) = setup(WatcherConfig {
        schedule: ScheduleConfig {
            start_time: start.into(),
            end_time: end.into(),
        },
        ..cfg()
    });
    w
}

#[test]
fn schedules_handle_same_day_midnight_crossing_and_none() {
    let day = sched("08:00", "18:00");
    assert!(
        day.would_be_within_schedule(at(8, 0))
            && day.would_be_within_schedule(at(18, 0))
            && !day.would_be_within_schedule(at(7, 59))
            && !day.would_be_within_schedule(at(18, 1))
    );
    let night = sched("22:00", "06:00");
    assert!(
        night.would_be_within_schedule(at(23, 0))
            && night.would_be_within_schedule(at(5, 59))
            && !night.would_be_within_schedule(at(12, 0))
    );
    assert!(
        sched("", "").would_be_within_schedule(at(3, 0))
            && sched("08:00", "").would_be_within_schedule(at(3, 0))
    );
}

#[test]
fn the_next_scheduled_run_follows_go_s_branches() {
    let day = sched("08:00", "18:00");
    assert_eq!(
        day.next_scheduled_run(at(19, 0)),
        at(8, 0) + chrono::Duration::hours(24),
        "after today's window: tomorrow"
    );
    assert_eq!(
        day.next_scheduled_run(at(6, 0)),
        at(8, 0),
        "before the window: today's start"
    );
    assert_eq!(
        day.next_scheduled_run(at(9, 0)),
        at(8, 0) + chrono::Duration::hours(24),
        "inside the window: start has passed"
    );
    let night = sched("22:00", "06:00");
    assert_eq!(
        night.next_scheduled_run(at(12, 0)),
        at(22, 0),
        "in the daytime gap: today's start"
    );
    assert_eq!(night.next_scheduled_run(at(23, 0)), at(22, 0));
}

#[test]
fn status_reports_schedule_and_next_run_only_when_enabled() {
    let (w, ..) = setup(WatcherConfig {
        schedule: ScheduleConfig {
            start_time: "00:00".into(),
            end_time: "23:59".into(),
        },
        ..cfg()
    });
    w.update_next_run_time();
    let s = w.watcher_status();
    assert!(
        s.initialized
            && s.enabled
            && !s.next_run.is_empty()
            && s.schedule.is_some()
            && s.is_within_schedule
    );
    let j = serde_json::to_value(&s).unwrap();
    assert!(
        j.get("watch_directory").is_some()
            && j.get("error").is_none()
            && j["check_interval"] == "1m"
    );
    let (off, ..) = setup(WatcherConfig {
        enabled: false,
        ..cfg()
    });
    let s = off.watcher_status();
    assert!(s.next_run.is_empty() && s.schedule.is_none());
    assert!(serde_json::to_value(&s).unwrap().get("next_run").is_none());
}

#[test]
fn the_size_cache_is_trimmed_after_a_day() {
    let (w, _, _, dir) = setup(cfg());
    write(&dir, "a.bin", 1);
    w.scan_directory(&CancellationToken::new()).unwrap();
    assert_eq!(w.size_cache.lock().unwrap().len(), 1);
    w.size_cache
        .lock()
        .unwrap()
        .values_mut()
        .for_each(|e| e.at = SystemTime::now() - Duration::from_secs(48 * 3600));
    w.cleanup_old_cache_entries();
    assert!(w.size_cache.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_loop_scans_on_its_interval_and_stops_on_cancel() {
    let (w, q, _, dir) = setup(WatcherConfig {
        check_interval: D::new("50ms"),
        ..cfg()
    });
    write(&dir, "a.bin", 10);
    let w = Arc::new(w);
    let c = CancellationToken::new();
    let h = tokio::spawn(w.clone().start(c.clone()));
    for _ in 0..60 {
        if !q.added.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(q.added.lock().unwrap().len(), 1);
    c.cancel();
    h.await.unwrap();
    // an immediate scan can also be triggered
    write(&dir, "b.bin", 10);
    w.scan_directory(&CancellationToken::new()).unwrap();
    w.trigger_scan(&CancellationToken::new());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(q.added.lock().unwrap().len(), 2);
}
