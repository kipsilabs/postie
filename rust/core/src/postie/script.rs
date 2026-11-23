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

const ENV_NZB: &str = "POSTIE_NZB_PATH";
const ENV_SOURCE: &str = "POSTIE_SOURCE_PATH";
const ENV_DIR: &str = "POSTIE_SOURCE_DIR";

fn source_dir(source_path: &str) -> String {
    let dir = Path::new(source_path)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir.is_empty() { ".".into() } else { dir }
}

/// POSIX `sh`: replaces each placeholder with a reference to an environment variable, so a file name
/// is never re-parsed as shell syntax. The reference is adapted to the quoting the placeholder sits in,
/// which keeps existing `script.sh "{nzb_path}"` commands working.
#[cfg(not(windows))]
pub fn expand_command(cmd: &str, _nzb_path: &str, _source_path: &str) -> String {
    const PLACEHOLDERS: [(&str, &str); 3] = [
        ("{nzb_path}", ENV_NZB),
        ("{source_path}", ENV_SOURCE),
        ("{source_dir}", ENV_DIR),
    ];
    let mut out = String::with_capacity(cmd.len());
    let (mut single, mut double) = (false, false);
    let mut rest = cmd;
    while let Some(c) = rest.chars().next() {
        if let Some((ph, var)) = PLACEHOLDERS.iter().find(|(ph, _)| rest.starts_with(ph)) {
            out.push_str(&match (single, double) {
                (true, _) => format!("'\"${var}\"'"),
                (_, true) => format!("${{{var}}}"),
                _ => format!("\"${{{var}}}\""),
            });
            rest = &rest[ph.len()..];
            continue;
        }
        match c {
            '\\' if !single => {
                out.push(c);
                rest = &rest[1..];
                if let Some(n) = rest.chars().next() {
                    out.push(n);
                    rest = &rest[n.len_utf8()..];
                }
                continue;
            }
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            _ => {}
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// `cmd /C` expands `%VAR%` before it parses `&` and `|`, so environment variables do not help: the
/// values are substituted literally and a value carrying cmd metacharacters is refused.
#[cfg(windows)]
pub fn expand_command(cmd: &str, nzb_path: &str, source_path: &str) -> String {
    let dir = source_dir(source_path);
    cmd.replace("{nzb_path}", nzb_path)
        .replace("{source_path}", source_path)
        .replace("{source_dir}", &dir)
}

#[cfg(windows)]
fn unsafe_for_cmd(v: &str) -> bool {
    v.chars().any(|c| {
        matches!(
            c,
            '&' | '|' | '<' | '>' | '^' | '%' | '"' | '\n' | '\r' | '`'
        )
    })
}

/// Kills the script's whole process group when dropped, so background grandchildren do not outlive it.
struct GroupKill(#[allow(dead_code)] Option<u32>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // the child leads its own group (process_group(0)), so its pid is the group id
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

const OUTPUT_CAP: usize = 64 * 1024;
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut r: R) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(n) = r.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        if buf.len() < OUTPUT_CAP {
            buf.extend_from_slice(&chunk[..n.min(OUTPUT_CAP - buf.len())]);
        }
    }
    buf
}

/// Runs the command and waits only for the shell itself: a background grandchild holding the pipes open
/// cannot stall us (output is collected for a short grace period), and every exit path kills the group.
async fn run_command(
    mut cmd: Command,
    cancel: &CancellationToken,
    timeout: Option<Duration>,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let _group = GroupKill(child.id());
    let mut out_task = tokio::spawn(read_capped(child.stdout.take().expect("piped stdout")));
    let mut err_task = tokio::spawn(read_capped(child.stderr.take().expect("piped stderr")));
    let status = tokio::select! {
        () = cancel.cancelled() => return Err("context canceled".into()),
        r = async {
            match timeout {
                Some(t) => tokio::time::timeout(t, child.wait())
                    .await
                    .map_err(|_| "signal: killed (timeout)".to_string())?
                    .map_err(|e| e.to_string()),
                None => child.wait().await.map_err(|e| e.to_string()),
            }
        } => r?,
    };
    // one shared grace period: waiting for the pipes one after the other would double it
    let (stdout, stderr) = tokio::join!(
        tokio::time::timeout(OUTPUT_GRACE, &mut out_task),
        tokio::time::timeout(OUTPUT_GRACE, &mut err_task),
    );
    let stdout = stdout.ok().and_then(Result::ok).unwrap_or_default();
    let stderr = stderr.ok().and_then(Result::ok).unwrap_or_default();
    out_task.abort();
    err_task.abort();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
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
    crate::logx::info(
        &format!(
            "Executing post upload script: command={} nzb_path={nzb_path} source_path={source_path} item_id={item_id}",
            cfg.command
        ),
        &[("component", "postie".into())],
    );
    #[cfg(windows)]
    if [nzb_path, source_path, &source_dir(source_path)]
        .into_iter()
        .any(|v| unsafe_for_cmd(v))
    {
        let msg = "post upload script refused: a path contains characters unsafe for cmd.exe";
        if let Some(q) = q
            && let Err(e) =
                q.update_script_status(item_id, "failed_permanent", 0, msg, None, Some(Utc::now()))
        {
            crate::logx::warn(
                &format!("Failed to track refused script: {e}"),
                &[("component", "postie".into())],
            );
        }
        return Err(msg.into());
    }
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
    cmd.env(ENV_NZB, nzb_path)
        .env(ENV_SOURCE, source_path)
        .env(ENV_DIR, source_dir(source_path));
    if let Some(d) = Path::new(nzb_path).parent() {
        cmd.current_dir(d);
    }
    let timeout = u64::try_from(cfg.timeout.nanos())
        .ok()
        .filter(|n| *n > 0)
        .map(Duration::from_nanos);
    let out: Result<std::process::Output, String> = run_command(cmd, cancel, timeout).await;
    let (ok, detail) = match out {
        Ok(o) => {
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
        Err(e) => (false, format!("{e}, output: ")),
    };
    if !ok {
        let error_msg = format!("script failed: {detail}");
        crate::logx::warn(
            &format!("Error executing post upload script: {detail} command={command}"),
            &[("component", "postie".into())],
        );
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
                crate::logx::warn(
                    &format!("Failed to track script failure: {e}"),
                    &[("component", "postie".into())],
                );
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
        crate::logx::warn(
            &format!("Failed to mark script as completed: {e}"),
            &[("component", "postie".into())],
        );
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

    #[cfg(not(windows))]
    #[test]
    fn placeholders_become_quoted_env_references() {
        let e = |c: &str| expand_command(c, "/o/a.nzb", "/s/dir/f.mkv");
        assert_eq!(e("x {nzb_path}"), "x \"${POSTIE_NZB_PATH}\"");
        assert_eq!(e("x \"{source_path}\""), "x \"${POSTIE_SOURCE_PATH}\"");
        assert_eq!(e("x '{source_dir}'"), "x ''\"$POSTIE_SOURCE_DIR\"''");
        assert_eq!(e("x \\{nzb_path}"), "x \\{nzb_path}");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn hostile_file_names_are_not_executed() {
        let d = std::env::temp_dir().join(format!("ps-inj-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&d).unwrap();
        let marker = d.join("pwned");
        let src = format!("/s/x$(touch {}).mkv", marker.display());
        run_post_upload_script(
            &CancellationToken::new(),
            &cfg("echo {source_path} \"{source_path}\" '{source_path}' > out.txt"),
            None,
            d.join("a.nzb").to_str().unwrap(),
            &src,
            "i",
        )
        .await
        .unwrap();
        assert!(!marker.exists());
        assert!(
            std::fs::read_to_string(d.join("out.txt"))
                .unwrap()
                .contains("$(touch")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_background_grandchild_holding_the_pipes_does_not_hang() {
        let t = std::time::Instant::now();
        let c = PostUploadScriptConfig {
            timeout: D::new("0s"),
            ..cfg("sleep 30 &")
        };
        run_post_upload_script(
            &CancellationToken::new(),
            &c,
            None,
            "/tmp/a.nzb",
            "/s/f",
            "i",
        )
        .await
        .unwrap();
        assert!(t.elapsed() < Duration::from_secs(10));
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
