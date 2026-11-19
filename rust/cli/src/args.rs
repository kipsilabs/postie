//! Flag parsing for `postie [watch]` (cobra flags of the Go CLI).

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub watch: bool,
    pub config: String,
    pub dir: String,
    pub verbose: bool,
    pub output_dir: String,
    pub input_file: String,
    pub ignore_patterns: Vec<String>,
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
    fn rejects_unknown_and_missing_value() {
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["-c"]).is_err());
    }
}
