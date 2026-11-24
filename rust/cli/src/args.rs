//! Flag parsing for `postie [watch]` (cobra flags of the Go CLI).

pub const USAGE: &str = "\
Upload files to Usenet once, or watch a directory and upload what appears in it.

Usage:
  postie [flags]          upload the files in --dir (or the single --input-file), then exit
  postie watch [flags]    keep watching --dir and upload new files

Flags:
  -c, --config <path>        Path to configuration file (default \"config.yaml\")
  -d, --dir <path>           Directory to upload or watch (default \".\")
  -i, --input-file <path>    Upload this single file instead of --dir
  -o, --output-dir <path>    Directory for the NZB files (default \".\")
      --ignore-pattern <glob>  Skip files whose name matches (repeatable; not with watch)
  -v, --verbose              Verbose logging
  -h, --help                 Show this help
      --version              Show the version
";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub watch: bool,
    pub config: String,
    pub dir: String,
    pub verbose: bool,
    pub output_dir: String,
    pub input_file: String,
    pub ignore_patterns: Vec<String>,
    pub help: bool,
    pub version: bool,
}

pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        config: "config.yaml".into(),
        dir: ".".into(),
        output_dir: ".".into(),
        ..Args::default()
    };
    let mut it = argv.into_iter().peekable();
    if it.peek().is_some_and(|s| s == "watch") {
        a.watch = true;
        it.next();
    }
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| format!("flag needs an argument: {name}"))
        };
        match flag.as_str() {
            "-c" | "--config" => a.config = value("--config")?,
            "-d" | "--dir" => a.dir = value("--dir")?,
            "-o" | "--output-dir" => a.output_dir = value("--output-dir")?,
            "-i" | "--input-file" => a.input_file = value("--input-file")?,
            "--ignore-pattern" => {
                if a.watch {
                    return Err("unknown flag: --ignore-pattern".into());
                }
                a.ignore_patterns.push(value("--ignore-pattern")?);
            }
            "-v" | "--verbose" => a.verbose = true,
            "-h" | "--help" => a.help = true,
            "--version" => a.version = true,
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &[&str]) -> Result<Args, String> {
        parse(s.iter().map(|x| x.to_string()))
    }

    #[test]
    fn defaults_match_go() {
        let a = p(&[]).unwrap();
        assert_eq!((a.config.as_str(), a.dir.as_str()), ("config.yaml", "."));
        assert!(!a.watch && !a.verbose);
    }

    #[test]
    fn parses_flags_and_watch() {
        let a = p(&["watch", "-c", "x.yaml", "--dir=/w", "-v"]).unwrap();
        assert!(a.watch && a.verbose);
        assert_eq!((a.config.as_str(), a.dir.as_str()), ("x.yaml", "/w"));
    }

    #[test]
    fn ignore_patterns_repeat_and_are_root_only() {
        let a = p(&["--ignore-pattern", "*.tmp", "--ignore-pattern=a?"]).unwrap();
        assert_eq!(a.ignore_patterns, ["*.tmp", "a?"]);
        assert!(p(&["watch", "--ignore-pattern", "x"]).is_err());
    }

    #[test]
    fn help_and_version_flags() {
        assert!(p(&["-h"]).unwrap().help);
        assert!(p(&["watch", "--help"]).unwrap().help);
        assert!(p(&["--version"]).unwrap().version);
        assert!(USAGE.contains("Path to configuration file"));
    }

    #[test]
    fn rejects_unknown_and_missing_value() {
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["-c"]).is_err());
    }
}
