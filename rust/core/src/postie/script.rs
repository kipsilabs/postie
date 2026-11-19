//! Port of `runPostUploadScript`: runs the configured post-upload script and tracks retry status.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::PostUploadScriptConfig;
use crate::queue::Queue;

/// The queue operations the script runner needs.
pub trait ScriptQueue: Send + Sync {
    fn update_script_status(
        &self,
        id: &str,
        status: &str,
        retry_count: i64,
        last_error: &str,
        next_retry_at: Option<chrono::DateTime<Utc>>,
        first_failure_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<(), String>;
    fn mark_script_completed(&self, id: &str) -> Result<(), String>;
}

impl ScriptQueue for Queue {
    fn update_script_status(
        &self,
        id: &str,
        status: &str,
        retry_count: i64,
        last_error: &str,
        next_retry_at: Option<chrono::DateTime<Utc>>,
        first_failure_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<(), String> {
        Queue::update_script_status(
            self,
            id,
            status,
            retry_count,
            last_error,
            next_retry_at,
            first_failure_at,
        )
        .map_err(|e| e.to_string())
    }
    fn mark_script_completed(&self, id: &str) -> Result<(), String> {
        Queue::mark_script_completed(self, id).map_err(|e| e.to_string())
    }
}

/// Replaces `{nzb_path}`, `{source_path}` and `{source_dir}`.
pub fn expand_command(cmd: &str, nzb_path: &str, source_path: &str) -> String {
    let dir = Path::new(source_path)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    cmd.replace("{nzb_path}", nzb_path)
        .replace("{source_path}", source_path)
        .replace("{source_dir}", if dir.is_empty() { "." } else { &dir })
}

/// A disabled or empty command is a no-op. On failure the item is scheduled for retry (when a queue is
/// given) and an error is returned; on success the item is marked completed.
pub async fn run_post_upload_script(
    cancel: &CancellationToken,
    cfg: &PostUploadScriptConfig,
    q: Option<&dyn ScriptQueue>,
    nzb_path: &str,
    source_path: &str,
    item_id: &str,
) -> Result<(), String> {
    if !cfg.enabled || cfg.command.is_empty() {
        return Ok(());
    }
    eprintln!(
        "postie: executing post upload script: command={} nzb_path={nzb_path} source_path={source_path} item_id={item_id}",
        cfg.command
    );
    let command = expand_command(&cfg.command, nzb_path, source_path);
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", &command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &command]);
        c
    };
    if let Some(d) = Path::new(nzb_path).parent() {
        cmd.current_dir(d);
    }
    // dropping the future kills the shell; leftover children holding the pipes are not waited on
    cmd.kill_on_drop(true);
    let timeout = u64::try_from(cfg.timeout.nanos())
        .ok()
        .filter(|n| *n > 0)
        .map(Duration::from_nanos);
    let run = cmd.output();
    let out: Result<std::io::Result<std::process::Output>, String> = tokio::select! {
        () = cancel.cancelled() => Err("context canceled".into()),
        r = async {
            match timeout {
                Some(t) => tokio::time::timeout(t, run).await.map_err(|_| "signal: killed (timeout)".to_string()),
                None => Ok(run.await),
            }
        } => r,
    };
    let (ok, detail) = match out {
        Ok(Ok(o)) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (
                o.status.success(),
                if o.status.success() {
                    text
                } else {
                    format!("{}, output: {text}", o.status)
                },
            )
        }
        Ok(Err(e)) => (false, format!("{e}, output: ")),
        Err(e) => (false, format!("{e}, output: ")),
    };
    if !ok {
        let error_msg = format!("script failed: {detail}");
        eprintln!("postie: error executing post upload script: {detail} command={command}");
        if let Some(q) = q {
            let now = Utc::now();
            let delay = chrono::Duration::nanoseconds(cfg.retry_delay.nanos().max(0));
            if let Err(e) = q.update_script_status(
                item_id,
                "pending_retry",
                0,
                &error_msg,
                Some(now + delay),
                Some(now),
            ) {
                eprintln!("postie: failed to track script failure: {e}");
            }
        }
        return Err(format!(
            "post upload script failed: {}",
            detail.split(", output:").next().unwrap_or(&detail)
        ));
    }
    if let Some(q) = q
        && let Err(e) = q.mark_script_completed(item_id)
    {
        eprintln!("postie: failed to mark script as completed: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that succeeds under both `sh -c` and `cmd /C`.
    const OK_CMD: &str = "exit 0";
    use crate::config::Duration as D;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Rec(Mutex<Vec<String>>);
    impl ScriptQueue for Rec {
        fn update_script_status(
            &self,
            id: &str,
            status: &str,
            rc: i64,
            err: &str,
            next: Option<chrono::DateTime<Utc>>,
            first: Option<chrono::DateTime<Utc>>,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push(format!(
                "update {id} {status} {rc} {} {} {}",
                err.contains("script failed"),
                next.is_some(),
                first.is_some()
            ));
            Ok(())
        }
        fn mark_script_completed(&self, id: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("done {id}"));
            Ok(())
        }
    }

    fn cfg(command: &str) -> PostUploadScriptConfig {
        PostUploadScriptConfig {
            enabled: true,
            command: command.into(),
            timeout: D::new("5s"),
            retry_delay: D::new("1m"),
            ..Default::default()
        }
    }

    #[test]
    fn placeholders_expand() {
        assert_eq!(
            expand_command(
                "x {nzb_path} {source_path} {source_dir}",
                "/o/a.nzb",
                "/s/dir/f.mkv"
            ),
            "x /o/a.nzb /s/dir/f.mkv /s/dir"
        );
    }

    #[tokio::test]
    async fn disabled_or_empty_is_a_no_op_and_success_marks_completed() {
        let q = Rec::default();
        let c = CancellationToken::new();
        run_post_upload_script(
            &c,
            &PostUploadScriptConfig {
                enabled: false,
                ..cfg(OK_CMD)
            },
            Some(&q),
            "/o/a.nzb",
            "/s/f",
            "i",
        )
        .await
        .unwrap();
        run_post_upload_script(&c, &cfg(""), Some(&q), "/o/a.nzb", "/s/f", "i")
            .await
            .unwrap();
        assert!(q.0.lock().unwrap().is_empty());
        let nzb = std::env::temp_dir().join("a.nzb");
        run_post_upload_script(
            &c,
            &cfg(OK_CMD),
            Some(&q),
            nzb.to_str().unwrap(),
            "/s/f",
            "item1",
        )
        .await
        .unwrap();
        assert_eq!(q.0.lock().unwrap().as_slice(), ["done item1"]);
    }

    #[tokio::test]
    async fn failure_schedules_a_retry_and_a_timeout_does_not_hang() {
        let q = Rec::default();
        let c = CancellationToken::new();
        let e = run_post_upload_script(
            &c,
            &cfg("echo bad; exit 2"),
            Some(&q),
            "/tmp/a.nzb",
            "/s/f",
            "item1",
        )
        .await
        .unwrap_err();
        assert!(e.starts_with("post upload script failed"), "{e}");
        assert_eq!(
            q.0.lock().unwrap().as_slice(),
            ["update item1 pending_retry 0 true true true"]
        );
        let slow = PostUploadScriptConfig {
            timeout: D::new("100ms"),
            ..cfg("sleep 30")
        };
        let t = std::time::Instant::now();
        assert!(
            run_post_upload_script(&c, &slow, None, "/tmp/a.nzb", "/s/f", "i")
                .await
                .is_err()
        );
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn the_command_runs_in_the_nzb_directory_with_expanded_paths() {
        let d = std::env::temp_dir().join(format!(
            "ps-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let nzb = d.join("a.nzb");
        run_post_upload_script(
            &CancellationToken::new(),
            &cfg(if cfg!(windows) {
                "cd > out.txt & echo {source_dir} >> out.txt"
            } else {
                "pwd > out.txt; echo {source_dir} >> out.txt"
            }),
            None,
            nzb.to_str().unwrap(),
            "/src/dir/f.mkv",
            "i",
        )
        .await
        .unwrap();
        let out = std::fs::read_to_string(d.join("out.txt")).unwrap();
        assert!(
            out.lines()
                .next()
                .unwrap()
                .ends_with(d.file_name().unwrap().to_str().unwrap())
                && out.contains("/src/dir"),
            "{out}"
        );
    }
}
