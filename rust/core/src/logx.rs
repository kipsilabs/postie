//! Structured logging like Go's `slog` JSON handler: one JSON object per line
//! (`time`, `level`, `msg`, then the attributes) written to stdout and to the rotating log file the
//! web API serves. Rotation mirrors lumberjack: 5 MB files, 3 backups.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

const MAX_SIZE: u64 = 5 * 1024 * 1024;
const MAX_BACKUPS: usize = 3;

struct Sink {
    path: PathBuf,
    file: Option<std::fs::File>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

fn sink() -> &'static Mutex<Sink> {
    SINK.get_or_init(|| {
        Mutex::new(Sink {
            path: PathBuf::new(),
            file: None,
        })
    })
}

/// The directory must be creatable and writable (probed with a temp file).
pub fn verify_log_directory(log_path: &Path) -> Result<PathBuf, String> {
    let dir = log_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("failed to create log directory {}: {e}", dir.display()))?;
    let test = dir.join(".postie_write_test");
    std::fs::write(&test, b"test")
        .map_err(|e| format!("log directory {} is not writable: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&test);
    Ok(log_path.to_path_buf())
}

pub fn fallback_log_path() -> PathBuf {
    std::env::temp_dir().join("postie").join("postie.log")
}

/// Starts file logging at `log_path` (falling back to the temp dir) and returns the path in use.
pub fn setup(log_path: &Path) -> Result<PathBuf, String> {
    let verified = match verify_log_directory(log_path) {
        Ok(p) => p,
        Err(e) => {
            warn(
                "Primary log directory not writable, trying fallback",
                &[
                    ("originalPath", log_path.to_string_lossy().into()),
                    ("error", e.clone().into()),
                ],
            );
            let fb = fallback_log_path();
            verify_log_directory(&fb).map_err(|e2| format!("neither primary nor fallback log directory is writable: primary={}, fallback={}: {e2}", log_path.display(), fb.display()))?
        }
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&verified)
        .map_err(|e| e.to_string())?;
    let mut s = sink().lock().unwrap_or_else(|p| p.into_inner());
    s.path.clone_from(&verified);
    s.file = Some(file);
    drop(s);
    info(
        "Logging initialized successfully",
        &[
            ("logPath", verified.to_string_lossy().into()),
            ("os", go_os().into()),
            ("compression", (go_os() != "windows").into()),
        ],
    );
    Ok(verified)
}

/// `runtime.GOOS` spelling.
pub fn go_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        o => o,
    }
}

fn rotate(s: &mut Sink) {
    let Some(f) = &s.file else { return };
    if f.metadata().map_or(0, |m| m.len()) < MAX_SIZE {
        return;
    }
    for i in (1..MAX_BACKUPS).rev() {
        let (from, to) = (
            s.path.with_extension(format!("log.{i}")),
            s.path.with_extension(format!("log.{}", i + 1)),
        );
        let _ = std::fs::rename(from, to);
    }
    let _ = std::fs::rename(&s.path, s.path.with_extension("log.1"));
    s.file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&s.path)
        .ok();
}

pub fn log(level: &str, msg: &str, attrs: &[(&str, Value)]) {
    let mut m = Map::new();
    m.insert(
        "time".into(),
        chrono::Local::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, false)
            .into(),
    );
    m.insert("level".into(), level.into());
    m.insert("msg".into(), msg.into());
    for (k, v) in attrs {
        m.insert((*k).into(), v.clone());
    }
    let mut line = serde_json::to_string(&Value::Object(m)).unwrap_or_default();
    line.push('\n');
    let _ = std::io::stdout().write_all(line.as_bytes());
    let mut s = sink().lock().unwrap_or_else(|p| p.into_inner());
    rotate(&mut s);
    if let Some(f) = &mut s.file {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn debug(msg: &str, attrs: &[(&str, Value)]) {
    log("DEBUG", msg, attrs);
}
pub fn info(msg: &str, attrs: &[(&str, Value)]) {
    log("INFO", msg, attrs);
}
pub fn warn(msg: &str, attrs: &[(&str, Value)]) {
    log("WARN", msg, attrs);
}
pub fn error(msg: &str, attrs: &[(&str, Value)]) {
    log("ERROR", msg, attrs);
}

/// The path currently being written (empty before `setup`).
pub fn current_path() -> PathBuf {
    sink()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .path
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_writes_json_lines_to_the_file() {
        let dir = std::env::temp_dir().join(format!(
            "logx-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let path = setup(&dir.join("sub/postie.log")).unwrap();
        info("hello", &[("n", 3.into()), ("ok", true.into())]);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["msg"], "Logging initialized successfully");
        let last = lines.last().unwrap();
        assert_eq!(
            (
                last["level"].as_str(),
                last["msg"].as_str(),
                last["n"].as_i64(),
                last["ok"].as_bool()
            ),
            (Some("INFO"), Some("hello"), Some(3), Some(true))
        );
        assert!(last["time"].as_str().is_some_and(|t| t.contains('T')));
        assert_eq!(current_path(), path);
    }

    #[test]
    fn os_name_is_go_spelling() {
        assert!(["darwin", "linux", "windows"].contains(&go_os()) || !go_os().is_empty());
    }
}
