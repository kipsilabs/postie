//! Port of `config_location.go`: the saved configuration location lives next to the default config so
//! it can be found before the YAML is loaded; relocation copies the config atomically.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

fn location_file(default_path: &Path) -> PathBuf {
    default_path
        .parent()
        .map(|d| d.join("config-location"))
        .unwrap_or_else(|| PathBuf::from("config-location"))
}

/// Go's `filepath.Clean` for absolute paths.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// The active config path: the saved location when one exists (must be absolute), else the default.
pub fn resolve_config_path(default_path: &Path) -> Result<PathBuf, String> {
    match std::fs::read_to_string(location_file(default_path)) {
        Ok(s) => {
            if !Path::new(&s).is_absolute() {
                return Err("saved configuration location must be an absolute path".into());
            }
            Ok(clean(Path::new(&s)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_path.to_path_buf()),
        Err(e) => Err(format!("read configuration location: {e}")),
    }
}

fn persist_location(file: &Path, destination: &Path) -> Result<(), String> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".config-location-{}", rand::random::<u32>()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(destination.to_string_lossy().as_bytes())?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("remember configuration location: {e}"));
    }
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("remember configuration location: {e}")
    })
}

/// Copies the current config into `directory/config.yaml` (never overwriting) and remembers the new
/// location; a failed step removes the copy. Returns the active path.
pub fn relocate_config(
    default_config: &Path,
    current: &Path,
    directory: &Path,
) -> Result<PathBuf, String> {
    if directory.as_os_str().is_empty() {
        return Err("configuration directory is required".into());
    }
    let directory = std::path::absolute(directory).map_err(|e| e.to_string())?;
    let destination = directory.join("config.yaml");
    let source = std::path::absolute(current).map_err(|e| e.to_string())?;
    if source == destination {
        return Ok(current.to_path_buf());
    }
    let data = std::fs::read(&source).map_err(|e| format!("read current configuration: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|e| {
            format!(
                "create configuration at {} (choose a directory without config.yaml): {e}",
                destination.display()
            )
        })?;
    let copied = file.write_all(&data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&destination);
        return Err(format!("copy configuration: {e}"));
    }
    if let Err(e) = persist_location(&location_file(default_config), &destination) {
        let _ = std::fs::remove_file(&destination);
        return Err(e);
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cl-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resolves_default_saved_and_rejects_relative_locations() {
        let d = tmp();
        let default = d.join("config.yaml");
        assert_eq!(resolve_config_path(&default).unwrap(), default);
        std::fs::write(d.join("config-location"), "/srv/cfg/../app/config.yaml").unwrap();
        assert_eq!(
            resolve_config_path(&default).unwrap(),
            PathBuf::from("/srv/app/config.yaml")
        );
        std::fs::write(d.join("config-location"), "relative/config.yaml").unwrap();
        assert!(
            resolve_config_path(&default)
                .unwrap_err()
                .contains("must be an absolute path")
        );
    }

    #[test]
    fn relocation_copies_remembers_and_refuses_to_overwrite() {
        let d = tmp();
        let default = d.join("config.yaml");
        std::fs::write(&default, "version: 3\n").unwrap();
        let target = tmp();
        let active = relocate_config(&default, &default, &target).unwrap();
        assert_eq!(active, target.join("config.yaml"));
        assert_eq!(std::fs::read_to_string(&active).unwrap(), "version: 3\n");
        assert_eq!(
            resolve_config_path(&default).unwrap(),
            active,
            "remembered for the next start"
        );
        assert_eq!(
            std::fs::read_dir(&d)
                .unwrap()
                .flatten()
                .filter(|e| e
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".config-location-"))
                .count(),
            0,
            "no temp file left"
        );
        // the same place again is a no-op; an occupied destination is refused and left alone
        assert_eq!(relocate_config(&default, &active, &target).unwrap(), active);
        let other = tmp();
        std::fs::write(other.join("config.yaml"), "keep").unwrap();
        let e = relocate_config(&default, &active, &other).unwrap_err();
        assert!(e.contains("choose a directory without config.yaml"), "{e}");
        assert_eq!(
            std::fs::read_to_string(other.join("config.yaml")).unwrap(),
            "keep"
        );
        assert_eq!(
            relocate_config(&default, &active, Path::new("")).unwrap_err(),
            "configuration directory is required"
        );
        assert!(
            relocate_config(&default, &d.join("missing.yaml"), &tmp())
                .unwrap_err()
                .starts_with("read current configuration")
        );
    }
}
