use std::path::PathBuf;
use std::time::Duration;

use super::*;

/// Writes `<out>/<base>.par2` per call after `delay_ms` (honouring cancellation) and counts calls.
struct Fake {
    out: PathBuf,
    delay_ms: u64,
    calls: AtomicUsize,
}

impl Fake {
    fn new(out: &Path, delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            out: out.to_path_buf(),
            delay_ms,
            calls: AtomicUsize::new(0),
        })
    }

    async fn make(&self, cancel: &CancellationToken, base: &str) -> Result<Par2Result, ExecError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(self.delay_ms)) => {}
            () = cancel.cancelled() => return Err(ExecError::Cancelled),
        }
        let p = self.out.join(format!("{base}.par2"));
        std::fs::write(&p, b"set").unwrap();
        Ok(Par2Result {
            created: vec![p.to_string_lossy().into_owned()],
            reused: Vec::new(),
        })
    }
}

impl Par2Executor for Fake {
    fn create<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _files: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(async { Ok(Par2Result::default()) })
    }
    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        _output_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        let base = Path::new(&files[0].path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        Box::pin(async move { self.make(cancel, &base).await })
    }
    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        _files: &'a [FileInfo],
        _output_dir: &'a str,
        set_name: &'a str,
        _folder_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(async move { self.make(cancel, set_name).await })
    }
}

struct Env {
    src: PathBuf,
    out: PathBuf,
    held: Arc<Mutex<Vec<String>>>,
    store: Arc<Prefetcher>,
    triggers: Arc<AtomicUsize>,
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "prefetch-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env() -> Env {
    let held = Arc::new(Mutex::new(Vec::<String>::new()));
    let h = held.clone();
    let store = Arc::new(Prefetcher::new(Arc::new(move |paths, keep| {
        let mut v = h.lock().unwrap();
        for p in paths {
            if keep {
                v.push(p.clone());
            } else if let Some(i) = v.iter().position(|x| x == p) {
                v.remove(i);
            }
        }
    })));
    let triggers = Arc::new(AtomicUsize::new(0));
    let t = triggers.clone();
    store.set_trigger(Arc::new(move || {
        t.fetch_add(1, Ordering::SeqCst);
    }));
    Env {
        src: tmp(),
        out: tmp(),
        held,
        store,
        triggers,
    }
}

impl Env {
    fn file(&self, name: &str) -> FileInfo {
        let p = self.src.join(name);
        std::fs::write(&p, name.as_bytes()).unwrap();
        FileInfo {
            path: p.to_string_lossy().into_owned(),
            size: name.len() as u64,
            relative_path: String::new(),
        }
    }

    fn job_executor(&self, inner: &Arc<Fake>) -> Arc<dyn Par2Executor> {
        PrefetchingExecutor::wrap(inner.clone(), self.store.clone(), None)
    }
}

fn req(file: &FileInfo) -> Par2Request {
    Par2Request::File {
        file: file.clone(),
        output_dir: String::new(),
    }
}

async fn wait_finished(store: &Prefetcher) {
    for _ in 0..200 {
        if lock(&store.slot)
            .as_ref()
            .is_none_or(|e| e.handle.is_finished())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("prefetch did not finish");
}

#[tokio::test]
async fn a_finished_prefetch_serves_the_job_call_without_running_the_executor() {
    let e = env();
    let f = e.file("a.bin");
    let pre = Fake::new(&e.out, 0);
    assert!(e.store.start("m1", req(&f), pre.clone()).is_none());
    wait_finished(&e.store).await;
    assert_eq!(e.held.lock().unwrap().len(), 1, "held from the sweeper");

    let inner = Fake::new(&e.out, 0);
    let res = e
        .job_executor(&inner)
        .create_in_directory(&CancellationToken::new(), std::slice::from_ref(&f), "")
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        res.created.len(),
        1,
        "handed over as created, so the job owns it"
    );
    assert!(Path::new(&res.created[0]).exists());
    assert_eq!(e.store.hits(), 1);
    assert!(
        e.held.lock().unwrap().is_empty(),
        "the job's tracking takes over"
    );
    assert_eq!(
        e.triggers.load(Ordering::SeqCst),
        1,
        "the next prefetch is queued"
    );
}

#[tokio::test]
async fn a_job_waits_for_an_in_flight_prefetch_instead_of_starting_over() {
    let e = env();
    let f = e.file("a.bin");
    let pre = Fake::new(&e.out, 150);
    let _ = e.store.start("m1", req(&f), pre.clone());
    let inner = Fake::new(&e.out, 0);
    let res = e
        .job_executor(&inner)
        .create_in_directory(&CancellationToken::new(), std::slice::from_ref(&f), "")
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(pre.calls.load(Ordering::SeqCst), 1);
    assert_eq!(res.created.len(), 1);
}

#[tokio::test]
async fn a_different_request_runs_the_executor_and_keeps_the_prefetch() {
    let e = env();
    let (a, b) = (e.file("a.bin"), e.file("b.bin"));
    let _ = e.store.start("m1", req(&a), Fake::new(&e.out, 0));
    wait_finished(&e.store).await;
    let inner = Fake::new(&e.out, 0);
    e.job_executor(&inner)
        .create_in_directory(&CancellationToken::new(), std::slice::from_ref(&b), "")
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(e.store.owner().as_deref(), Some("m1"));
    assert_eq!(e.store.hits(), 0);
}

#[tokio::test]
async fn changed_inputs_delete_the_stale_set_before_the_job_creates_a_fresh_one() {
    let e = env();
    let f = e.file("a.bin");
    let _ = e.store.start("m1", req(&f), Fake::new(&e.out, 0));
    wait_finished(&e.store).await;
    let stale = e.out.join("a.bin.par2");
    assert!(stale.exists());
    // the source changes after the prefetch
    let t = SystemTime::now() + Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&f.path)
        .unwrap()
        .set_modified(t)
        .unwrap();

    // the job's executor writes elsewhere, so the stale file must be gone by the time it runs
    let job_out = tmp();
    let inner = Fake::new(&job_out, 0);
    e.job_executor(&inner)
        .create_in_directory(&CancellationToken::new(), std::slice::from_ref(&f), "")
        .await
        .unwrap();
    assert!(!stale.exists(), "stale prefetched set deleted");
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(e.store.hits(), 0);
    assert!(e.held.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discarding_another_jobs_prefetch_deletes_its_files_and_releases_them() {
    let e = env();
    let f = e.file("a.bin");
    let _ = e.store.start("m1", req(&f), Fake::new(&e.out, 0));
    wait_finished(&e.store).await;
    assert!(
        e.store.take_unless_owner("m1").is_none(),
        "own prefetch kept"
    );
    e.store
        .take_unless_owner("m2")
        .expect("someone else's prefetch removed")
        .discard()
        .await;
    assert!(!e.out.join("a.bin.par2").exists());
    assert!(e.held.lock().unwrap().is_empty());
    assert!(e.store.owner().is_none());
}

#[tokio::test]
async fn replacing_an_in_flight_prefetch_cancels_it_and_leaves_no_files() {
    let e = env();
    let (a, b) = (e.file("a.bin"), e.file("b.bin"));
    let slow = Fake::new(&e.out, 5_000);
    let _ = e.store.start("m1", req(&a), slow);
    let old = e.store.start("m2", req(&b), Fake::new(&e.out, 0));
    tokio::time::timeout(Duration::from_secs(2), old.expect("replaced").discard())
        .await
        .expect("cancelled promptly");
    assert!(!e.out.join("a.bin.par2").exists());
    assert_eq!(e.store.owner().as_deref(), Some("m2"));
}

#[tokio::test]
async fn the_same_request_is_not_prefetched_twice() {
    let e = env();
    let f = e.file("a.bin");
    let pre = Fake::new(&e.out, 0);
    let _ = e.store.start("m1", req(&f), pre.clone());
    wait_finished(&e.store).await;
    assert!(e.store.start("m1", req(&f), pre.clone()).is_none());
    wait_finished(&e.store).await;
    assert_eq!(pre.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_job_cancelled_while_waiting_stops_the_prefetch_and_keeps_nothing() {
    let e = env();
    let f = e.file("a.bin");
    let _ = e.store.start("m1", req(&f), Fake::new(&e.out, 5_000));
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c2.cancel();
    });
    let inner = Fake::new(&e.out, 0);
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        e.job_executor(&inner)
            .create_in_directory(&cancel, std::slice::from_ref(&f), ""),
    )
    .await
    .expect("returns promptly");
    assert_eq!(res, Err(ExecError::Cancelled));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(!e.out.join("a.bin.par2").exists());
    assert_eq!(
        e.triggers.load(Ordering::SeqCst),
        0,
        "a cancelled job queues nothing"
    );
}

#[tokio::test]
async fn folder_sets_are_prefetched_too() {
    let e = env();
    let files = vec![e.file("a.bin"), e.file("b.bin")];
    let set = Par2Request::Set {
        files: files.clone(),
        output_dir: String::new(),
        set_name: "Folder".into(),
        folder_dir: e.src.to_string_lossy().into_owned(),
    };
    let _ = e.store.start("m1", set, Fake::new(&e.out, 0));
    wait_finished(&e.store).await;
    let inner = Fake::new(&e.out, 0);
    let res = e
        .job_executor(&inner)
        .create_set(
            &CancellationToken::new(),
            &files,
            "",
            "Folder",
            &e.src.to_string_lossy(),
        )
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(res.created[0].ends_with("Folder.par2"));
}

#[tokio::test]
async fn a_changed_request_for_the_same_set_file_is_discarded_before_the_job_writes_it() {
    let e = env();
    let (a, b) = (e.file("a.bin"), e.file("b.bin"));
    let dir = e.src.to_string_lossy().into_owned();
    let set = |files: Vec<FileInfo>| Par2Request::Set {
        files,
        output_dir: String::new(),
        set_name: "Folder".into(),
        folder_dir: dir.clone(),
    };
    let _ = e
        .store
        .start("m1", set(vec![a.clone()]), Fake::new(&e.out, 0));
    wait_finished(&e.store).await;
    // the folder gained a file before the job ran: same set file, different request
    let inner = Fake::new(&e.out, 0);
    let res = e
        .job_executor(&inner)
        .create_set(&CancellationToken::new(), &[a, b], "", "Folder", &dir)
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert!(e.store.owner().is_none(), "the stale prefetch is gone");
    // a later prefetch for another job must not delete the job's own set
    let c = e.file("c.bin");
    if let Some(old) = e.store.start("m2", req(&c), Fake::new(&e.out, 0)) {
        old.discard().await;
    }
    assert!(
        Path::new(&res.created[0]).exists(),
        "the job's set survives"
    );
}
