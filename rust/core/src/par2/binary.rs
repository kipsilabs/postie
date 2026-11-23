//! Port of `internal/par2/binary.go`: PAR2 creation by shelling out to an external ParPar binary
//! (`par2.parpar_binary_path`), with progress parsed from its `Finished : 23.45%` lines.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::executor::{
    ExecError, Par2Executor, calculate_par_block_size, parse_redundancy_percentage,
};
use super::existing::{
    Par2Result, check_existing_par2_files_in_path, check_existing_par2_set_in_path,
    par2_set_on_disk, set_input_names, work_dir,
};
use crate::config::Par2Config;
use crate::fileinfo::FileInfo;
use crate::poster::BoxFut;
use crate::progress::{JobProgress, Progress, ProgressType};

fn msg<T>(s: impl Into<String>) -> Result<T, ExecError> {
    Err(ExecError::Msg(s.into()))
}

fn base_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parent(p: &str) -> PathBuf {
    Path::new(p)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn is_par2_ext(path: &str) -> bool {
    base_name(path)
        .rfind('.')
        .is_some_and(|i| base_name(path)[i..] == *".par2")
}

/// The percentage in a `Finished   : 23.45%` line, if the line is one.
fn parse_progress_line(line: &str) -> Option<f64> {
    let rest = line
        .trim_start()
        .strip_prefix("Finished")?
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    if end == 0 || !rest[end..].starts_with('%') {
        return None;
    }
    rest[..end].parse().ok()
}

pub struct BinaryExecutor {
    article_size: u64,
    cfg: Par2Config,
    progress: Option<Arc<JobProgress>>,
}

impl BinaryExecutor {
    pub fn new(article_size: u64, cfg: Par2Config, progress: Option<Arc<JobProgress>>) -> Self {
        Self {
            article_size,
            cfg,
            progress,
        }
    }

    fn resolve_dir(&self, file: &FileInfo, output_dir: &str) -> PathBuf {
        if !output_dir.is_empty() {
            if std::fs::create_dir_all(output_dir).is_ok() {
                return PathBuf::from(output_dir);
            }
        } else {
            let wd = work_dir(&self.cfg);
            if !wd.is_empty() && std::fs::create_dir_all(&wd).is_ok() {
                return PathBuf::from(wd);
            }
        }
        parent(&file.path)
    }

    /// Runs the binary, streaming `\r`/`\n`-separated output for progress; returns on exit.
    async fn run(
        &self,
        cancel: &CancellationToken,
        label: &str,
        args: Vec<String>,
    ) -> Result<(), ExecError> {
        let id = Uuid::new_v4();
        let bar: Option<Arc<Progress>> = self.progress.as_ref().map(|p| {
            p.add_progress(
                id,
                &format!("PAR2: {label}"),
                ProgressType::Par2Generation,
                100,
            )
        });
        let finish = || {
            if let Some(p) = &self.progress {
                p.finish_progress(id);
            }
        };
        let mut cmd = Command::new(&self.cfg.parpar_binary_path);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                finish();
                return msg(format!("parpar start for {label}: {e}"));
            }
        };
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| ExecError::Msg("parpar stdout pipe".into()))?,
        );
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| ExecError::Msg("parpar stderr pipe".into()))?;
        let err_task = tokio::spawn(async move {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s).await;
            s
        });
        let mut all_out = String::new();
        let mut line = Vec::new();
        let mut last_pct: i64 = 0;
        let mut buf = [0u8; 4096];
        loop {
            let n = tokio::select! {
                () = cancel.cancelled() => {
                    let _ = child.kill().await;
                    finish();
                    return Err(ExecError::Cancelled);
                }
                r = stdout.read(&mut buf) => r.unwrap_or(0),
            };
            if n == 0 {
                break;
            }
            all_out.push_str(&String::from_utf8_lossy(&buf[..n]));
            for &b in &buf[..n] {
                if b == b'\r' || b == b'\n' {
                    if let (Some(pg), Some(pct)) =
                        (&bar, parse_progress_line(&String::from_utf8_lossy(&line)))
                    {
                        let new = pct as i64;
                        if new > last_pct {
                            pg.update_progress(new - last_pct);
                            last_pct = new;
                        }
                    }
                    line.clear();
                } else {
                    line.push(b);
                }
            }
        }
        let status = child.wait().await;
        let stderr_text = err_task.await.unwrap_or_default();
        if cancel.is_cancelled() {
            finish();
            return Err(ExecError::Cancelled);
        }
        match status {
            Ok(s) if s.success() => {
                if let Some(pg) = &bar
                    && last_pct < 100
                {
                    pg.update_progress(100 - last_pct);
                }
                finish();
                Ok(())
            }
            other => {
                finish();
                let mut combined = all_out.trim().to_string();
                let e = stderr_text.trim();
                if !e.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(e);
                }
                let why = other.map_or_else(|e| e.to_string(), |s| s.to_string());
                msg(format!(
                    "parpar failed for {label}: {why}\noutput: {combined}"
                ))
            }
        }
    }

    async fn run_parpar(
        &self,
        cancel: &CancellationToken,
        file: &FileInfo,
        dir: &Path,
    ) -> Result<Vec<String>, ExecError> {
        let block = calculate_par_block_size(file.size, self.article_size);
        if block == 0 {
            return msg(format!("parpar: no usable block size for {}", file.path));
        }
        let pct = parse_redundancy_percentage(&self.cfg.redundancy);
        let slices = file.size.div_ceil(block).max(1);
        let recovery = ((slices as f64 * pct / 100.0).ceil() as usize).max(1);
        let base = base_name(&file.path);
        let out_base = dir.join(&base).to_string_lossy().into_owned();
        let mut args = vec![
            "-s".to_string(),
            format!("{block}b"),
            "-r".into(),
            recovery.to_string(),
            "-o".into(),
            out_base.clone(),
        ];
        args.extend(self.cfg.parpar_extra_args.clone().unwrap_or_default());
        args.push(file.path.clone());
        self.run(cancel, &base, args).await.map_err(|e| match e {
            ExecError::Msg(m) => {
                ExecError::Msg(m.replace(&format!("for {base}:"), &format!("for {}:", file.path)))
            }
            other => other,
        })?;
        if !Path::new(&format!("{out_base}.par2")).exists() {
            crate::logx::warn(
                &format!("ParPar finished but main PAR2 file is missing: path={out_base}.par2"),
                &[("component", "par2".into())],
            );
            return Ok(Vec::new());
        }
        Ok(par2_set_on_disk(dir, &base))
    }

    async fn do_create_in_directory(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        output_dir: &str,
    ) -> Result<Par2Result, ExecError> {
        let mut res = Par2Result::default();
        for file in files {
            if is_par2_ext(&file.path) {
                continue;
            }
            let dir = self.resolve_dir(file, output_dir);
            let source_dir = parent(&file.path);
            if source_dir != dir
                && let Some(e) = check_existing_par2_files_in_path(file, &source_dir)
            {
                res.reused.extend(e);
                continue;
            }
            if let Some(e) = check_existing_par2_files_in_path(file, &dir) {
                res.reused.extend(e);
                continue;
            }
            res.created
                .extend(self.run_parpar(cancel, file, &dir).await?);
        }
        Ok(res)
    }

    async fn do_create_set(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        output_dir: &str,
        set_name: &str,
        folder_dir: &str,
    ) -> Result<Par2Result, ExecError> {
        if files.is_empty() {
            return msg(format!("par2: no input files for set {set_name:?}"));
        }
        if set_name.is_empty() {
            return msg("par2: empty set name");
        }
        let inputs: Vec<FileInfo> = files
            .iter()
            .filter(|f| !is_par2_ext(&f.path))
            .cloned()
            .collect();
        if inputs.is_empty() {
            return msg(format!(
                "par2: no non-par2 input files for set {set_name:?}"
            ));
        }
        let dir = if !output_dir.is_empty() {
            PathBuf::from(output_dir)
        } else if !work_dir(&self.cfg).is_empty() {
            PathBuf::from(work_dir(&self.cfg))
        } else {
            parent(&inputs[0].path)
        };
        std::fs::create_dir_all(&dir).map_err(|e| {
            ExecError::Msg(format!("par2: create output dir {}: {e}", dir.display()))
        })?;
        if let Some(existing) =
            check_existing_par2_set_in_path(set_name, &dir, &set_input_names(&inputs, folder_dir))
        {
            return Ok(Par2Result {
                reused: existing,
                created: Vec::new(),
            });
        }
        let total: u64 = inputs.iter().map(|f| f.size).sum();
        let max_file = inputs.iter().map(|f| f.size).max().unwrap_or(0);
        let mut block = calculate_par_block_size(total, self.article_size);
        if max_file > 0 && block > max_file {
            block = max_file / 4 * 4;
        }
        if block < 4 {
            crate::logx::info(
                &format!(
                    "Block size too small for PAR2 set creation, skipping: setName={set_name} totalSize={total}"
                ),
                &[("component", "par2".into())],
            );
            return Ok(Par2Result::default());
        }
        let slices: u64 = inputs.iter().map(|f| f.size.div_ceil(block).max(1)).sum();
        let pct = parse_redundancy_percentage(&self.cfg.redundancy);
        let recovery = ((slices as f64 * pct / 100.0).ceil() as usize).max(1);
        let mut args = vec![
            "-s".to_string(),
            format!("{block}b"),
            "-r".into(),
            recovery.to_string(),
            "-o".into(),
            dir.join(set_name).to_string_lossy().into_owned(),
            "--filepath-base".into(),
            folder_dir.into(),
            "--filepath-format".into(),
            "outrel".into(),
        ];
        args.extend(self.cfg.parpar_extra_args.clone().unwrap_or_default());
        args.extend(inputs.iter().map(|f| f.path.clone()));
        self.run(cancel, set_name, args)
            .await
            .map_err(|e| match e {
                ExecError::Msg(m) => ExecError::Msg(
                    m.replace(&format!("for {set_name}:"), &format!("for set {set_name}:")),
                ),
                other => other,
            })?;
        Ok(Par2Result {
            created: par2_set_on_disk(&dir, set_name),
            reused: Vec::new(),
        })
    }
}

impl Par2Executor for BinaryExecutor {
    fn create<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.do_create_in_directory(cancel, files, ""))
    }

    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.do_create_in_directory(cancel, files, output_dir))
    }

    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
        set_name: &'a str,
        folder_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.do_create_set(cancel, files, output_dir, set_name, folder_dir))
    }
}

/// Picks the executor from config: the ParPar binary when a path is set, else the native creator.
pub fn new_executor(
    article_size: u64,
    cfg: &Par2Config,
    progress: Option<Arc<JobProgress>>,
) -> Arc<dyn Par2Executor> {
    if cfg.parpar_binary_path.is_empty() {
        Arc::new(super::executor::NativeExecutor::new(
            article_size,
            cfg.clone(),
            progress,
        ))
    } else {
        Arc::new(BinaryExecutor::new(article_size, cfg.clone(), progress))
    }
}

// the fake parpar is a shell script
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "par2b-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A fake `parpar`: prints progress with carriage returns and writes the main + a volume file.
    fn fake_parpar(dir: &Path, body: &str) -> String {
        let p = dir.join("parpar.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn cfg(bin: String) -> Par2Config {
        Par2Config {
            parpar_binary_path: bin,
            redundancy: "10".into(),
            parpar_extra_args: Some(vec!["--extra".into()]),
            ..Default::default()
        }
    }

    fn src(dir: &Path) -> FileInfo {
        let p = dir.join("movie.bin");
        std::fs::write(&p, vec![7u8; 5000]).unwrap();
        FileInfo {
            path: p.to_string_lossy().into_owned(),
            size: 5000,
            relative_path: String::new(),
        }
    }

    #[test]
    fn progress_lines_are_recognised() {
        assert_eq!(
            parse_progress_line("Finished          : 23.45%"),
            Some(23.45)
        );
        assert_eq!(parse_progress_line("  Finished: 100%"), Some(100.0));
        for no in [
            "Finished : x%",
            "Finished 5%",
            "Done: 5%",
            "Finished : 5",
            "",
        ] {
            assert_eq!(parse_progress_line(no), None, "{no:?}");
        }
    }

    #[tokio::test]
    async fn runs_the_binary_with_the_expected_arguments_and_collects_the_set() {
        let d = tmp();
        let args_file = d.join("args.txt");
        let bin = fake_parpar(
            &d,
            &format!(
                "echo \"$@\" > {}\nprintf 'Finished : 10%%\\rFinished : 60%%\\rFinished : 100%%\\n'\nfor a in \"$@\"; do o=$prev; prev=$a; if [ \"$o\" = \"-o\" ]; then touch \"$a.par2\" \"$a.vol0+1.par2\"; fi; done",
                args_file.display()
            ),
        );
        let jp = Arc::new(JobProgress::new("j"));
        let ex = BinaryExecutor::new(750_000, cfg(bin), Some(jp.clone()));
        let f = src(&d);
        let res = ex
            .create_in_directory(&CancellationToken::new(), std::slice::from_ref(&f), "")
            .await
            .unwrap();
        let mut names: Vec<String> = res.created.iter().map(|p| base_name(p)).collect();
        names.sort();
        assert_eq!(names, ["movie.bin.par2", "movie.bin.vol0+1.par2"]);
        let args = std::fs::read_to_string(args_file).unwrap();
        assert!(
            args.starts_with("-s 750000b -r 1 -o ")
                && args.trim_end().ends_with(&format!("--extra {}", f.path)),
            "{args}"
        );
        assert!(jp.get_all_progress().is_empty());
    }

    #[tokio::test]
    async fn a_failing_binary_reports_its_output_and_a_missing_one_fails_to_start() {
        let d = tmp();
        let bin = fake_parpar(&d, "echo out-text; echo err-text >&2; exit 3");
        let f = src(&d);
        let e = BinaryExecutor::new(750_000, cfg(bin), None)
            .create(&CancellationToken::new(), std::slice::from_ref(&f))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with(&format!("parpar failed for {}:", f.path))
                && e.contains("output: out-text\nerr-text"),
            "{e}"
        );
        let e = BinaryExecutor::new(750_000, cfg("/nonexistent/parpar".into()), None)
            .create(&CancellationToken::new(), &[f])
            .await
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("parpar start for"), "{e}");
    }

    #[tokio::test]
    async fn cancellation_kills_the_process() {
        let d = tmp();
        let bin = fake_parpar(&d, "sleep 30");
        let c = CancellationToken::new();
        let c2 = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            c2.cancel();
        });
        let t = std::time::Instant::now();
        let e = BinaryExecutor::new(750_000, cfg(bin), None)
            .create(&c, &[src(&d)])
            .await
            .unwrap_err();
        assert_eq!(e, ExecError::Cancelled);
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn sets_pass_the_folder_base_and_missing_main_files_are_tolerated() {
        let d = tmp();
        let args_file = d.join("a.txt");
        let bin = fake_parpar(&d, &format!("echo \"$@\" > {}", args_file.display()));
        let f = src(&d);
        let ex = BinaryExecutor::new(750_000, cfg(bin), None);
        let res = ex
            .create_set(
                &CancellationToken::new(),
                std::slice::from_ref(&f),
                "",
                "Show",
                d.to_str().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.created.len(),
            1,
            "only the (absent) main path is listed"
        );
        let args = std::fs::read_to_string(args_file).unwrap();
        assert!(
            args.contains("--filepath-base")
                && args.contains("--filepath-format outrel")
                && args.contains("-o "),
            "{args}"
        );
        // single-file: a missing main file yields an empty result rather than an error
        let one = ex
            .create(&CancellationToken::new(), std::slice::from_ref(&f))
            .await
            .unwrap();
        assert!(one.created.is_empty());
        assert!(
            new_executor(1, &Par2Config::default(), None)
                .create_set(&CancellationToken::new(), &[], "", "x", "")
                .await
                .is_err()
        );
    }
}
