//! Port of `paths.go`: where config, database, data and log live per OS (and inside Docker).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub database: PathBuf,
    pub data: PathBuf,
    pub log: PathBuf,
}

/// Environment the path rules read (injectable for tests).
pub struct Env<'a> {
    pub os: &'a str,
    pub home: Option<PathBuf>,
    pub var: &'a dyn Fn(&str) -> Option<String>,
    pub docker: bool,
}

/// `/.dockerenv` or a cgroup file whose base name is `docker`/`containerd` (Go compares the base of the
/// whole file content, so in practice only `.dockerenv` triggers it).
pub fn is_running_in_docker() -> bool {
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    std::fs::read("/proc/1/cgroup").is_ok_and(|d| {
        let s = String::from_utf8_lossy(&d);
        let base = s.rsplit('/').next().unwrap_or("");
        !d.is_empty() && (base == "docker" || base == "containerd")
    })
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

/// The directories for this platform; `create` makes them (as `GetAppPaths` does).
pub fn app_paths_for(env: &Env<'_>) -> Result<AppPaths, String> {
    let home = || {
        env.home
            .clone()
            .ok_or_else(|| "$HOME is not defined".to_string())
    };
    let (config_dir, data_dir) = if env.docker {
        (PathBuf::from("/config"), PathBuf::from("/config"))
    } else {
        match env.os {
            "macos" => {
                let d = home()?
                    .join("Library")
                    .join("Application Support")
                    .join("Postie");
                (d.clone(), d)
            }
            "windows" => {
                let base = match nonempty((env.var)("APPDATA")) {
                    Some(a) => PathBuf::from(a),
                    None => home()?.join("AppData").join("Roaming"),
                };
                let d = base.join("Postie");
                (d.clone(), d)
            }
            _ => {
                let cfg = match nonempty((env.var)("XDG_CONFIG_HOME")) {
                    Some(c) => PathBuf::from(c),
                    None => home()?.join(".config"),
                };
                let data = match nonempty((env.var)("XDG_DATA_HOME")) {
                    Some(d) => PathBuf::from(d),
                    None => home()?.join(".local").join("share"),
                };
                (cfg.join("postie"), data.join("postie"))
            }
        }
    };
    Ok(AppPaths {
        config: config_dir.join("config.yaml"),
        database: config_dir.join("postie.db"),
        log: data_dir.join("postie.log"),
        data: data_dir,
    })
}

/// Paths for the running process, with the directories created.
pub fn get_app_paths() -> Result<AppPaths, String> {
    let env = Env {
        os: std::env::consts::OS,
        home: std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from),
        var: &|k| std::env::var(k).ok(),
        docker: is_running_in_docker(),
    };
    let p = app_paths_for(&env)?;
    let config_dir = p.config.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    if config_dir != p.data {
        std::fs::create_dir_all(&p.data).map_err(|e| e.to_string())?;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(os: &'a str, vars: &'a dyn Fn(&str) -> Option<String>, docker: bool) -> Env<'a> {
        Env {
            os,
            home: Some(PathBuf::from("/home/u")),
            var: vars,
            docker,
        }
    }

    #[test]
    fn linux_uses_xdg_with_separate_config_and_data() {
        let none = |_: &str| None;
        let p = app_paths_for(&env("linux", &none, false)).unwrap();
        assert_eq!(
            (
                p.config.to_str(),
                p.database.to_str(),
                p.data.to_str(),
                p.log.to_str()
            ),
            (
                Some("/home/u/.config/postie/config.yaml"),
                Some("/home/u/.config/postie/postie.db"),
                Some("/home/u/.local/share/postie"),
                Some("/home/u/.local/share/postie/postie.log")
            )
        );
        let xdg = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/x/cfg".into()),
            "XDG_DATA_HOME" => Some("/x/data".into()),
            _ => None,
        };
        let p = app_paths_for(&env("linux", &xdg, false)).unwrap();
        assert_eq!(
            (p.config.to_str(), p.log.to_str()),
            (
                Some("/x/cfg/postie/config.yaml"),
                Some("/x/data/postie/postie.log")
            )
        );
        let empty = |_: &str| Some(String::new());
        assert_eq!(
            app_paths_for(&env("linux", &empty, false))
                .unwrap()
                .config
                .to_str(),
            Some("/home/u/.config/postie/config.yaml"),
            "empty XDG vars fall back"
        );
    }

    #[test]
    fn macos_windows_and_docker() {
        let none = |_: &str| None;
        let m = app_paths_for(&env("macos", &none, false)).unwrap();
        assert_eq!(
            (m.config.to_str(), m.data.to_str()),
            (
                Some("/home/u/Library/Application Support/Postie/config.yaml"),
                Some("/home/u/Library/Application Support/Postie")
            )
        );
        let appdata = |k: &str| (k == "APPDATA").then(|| "/AppData".to_string());
        assert_eq!(
            app_paths_for(&env("windows", &appdata, false))
                .unwrap()
                .database
                .to_str(),
            Some("/AppData/Postie/postie.db")
        );
        assert_eq!(
            app_paths_for(&env("windows", &none, false))
                .unwrap()
                .data
                .to_str(),
            Some("/home/u/AppData/Roaming/Postie")
        );
        let d = app_paths_for(&env("linux", &none, true)).unwrap();
        assert_eq!(
            (d.config.to_str(), d.database.to_str(), d.log.to_str()),
            (
                Some("/config/config.yaml"),
                Some("/config/postie.db"),
                Some("/config/postie.log")
            )
        );
        assert!(
            app_paths_for(&Env {
                os: "linux",
                home: None,
                var: &none,
                docker: false
            })
            .is_err()
        );
    }
}
