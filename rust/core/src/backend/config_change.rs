//! Port of the change predicates in `backend/config.go`: after a settings save, only the components whose
//! captured-at-init inputs changed are restarted (pool, processor, watchers).

use std::collections::HashMap;

use crate::config::{ConfigData, WatcherConfig};
use crate::pool::{provider_key, server_config_changed};

fn servers(c: &ConfigData) -> &[crate::config::ServerConfig] {
    c.servers.as_deref().unwrap_or(&[])
}

fn watchers(c: &ConfigData) -> &[WatcherConfig] {
    c.watchers.as_deref().unwrap_or(&[])
}

/// Whether the provider list changed in a way that requires touching the NNTP pool. Only provider
/// fields count; reordering alone is no change (providers are matched by key, not index). A missing old
/// config always counts as a change.
pub fn pool_config_changed(old: Option<&ConfigData>, new: &ConfigData) -> bool {
    let Some(old) = old else { return true };
    let old_by_key: HashMap<String, &crate::config::ServerConfig> =
        servers(old).iter().map(|s| (provider_key(s), s)).collect();
    if servers(new).len() != old_by_key.len() {
        return true;
    }
    servers(new).iter().any(|n| {
        old_by_key
            .get(&provider_key(n))
            .is_none_or(|o| server_config_changed(o, n))
    })
}

/// Whether any PAR2 setting changed (they are captured per job, and `parpar_binary_path` selects the
/// executor, so any change reinitialises the processor).
pub fn par2_config_changed(old: Option<&ConfigData>, new: &ConfigData) -> bool {
    let Some(old) = old else { return true };
    old.par2.enabled != new.par2.enabled
        || old.par2.maintain_par2_files != new.par2.maintain_par2_files
        || old.par2.skip_if_par2_exists != new.par2.skip_if_par2_exists
        || old.par2.redundancy != new.par2.redundancy
        || old.par2.temp_dir != new.par2.temp_dir
        || old.par2.parpar_binary_path != new.par2.parpar_binary_path
        || old.par2.num_goroutines != new.par2.num_goroutines
        || old.par2.memory_limit != new.par2.memory_limit
        || old.par2.slice_size != new.par2.slice_size
        || old.par2.parpar_extra_args.clone().unwrap_or_default()
            != new.par2.parpar_extra_args.clone().unwrap_or_default()
}

/// Whether config captured by the processor at init changed. Pool-only changes do not count: the
/// processor shares the pool manager by reference.
pub fn processor_config_changed(old: Option<&ConfigData>, new: &ConfigData) -> bool {
    let Some(o) = old else { return true };
    if o.duplicate_lookup_config() != new.duplicate_lookup_config()
        || o.output_dir() != new.output_dir()
        || o.maintain_original_extension() != new.maintain_original_extension()
        || o.queue != new.queue
    {
        return true;
    }
    let (ow, nw) = (o.watcher_config(), new.watcher_config());
    if ow.delete_original_file != nw.delete_original_file
        || ow.min_file_age_to_delete != nw.min_file_age_to_delete
        || ow.watch_directory != nw.watch_directory
        || ow.follow_symlinks != nw.follow_symlinks
    {
        return true;
    }
    par2_config_changed(old, new)
}

/// Whether any watcher setting changed (list length, any field, or the ignore patterns).
pub fn watcher_config_changed(old: Option<&ConfigData>, new: &ConfigData) -> bool {
    let Some(old) = old else { return true };
    let (oc, nc) = (watchers(old), watchers(new));
    oc.len() != nc.len()
        || oc.iter().zip(nc).any(|(o, n)| {
            o.name != n.name
                || o.enabled != n.enabled
                || o.watch_directory != n.watch_directory
                || o.size_threshold != n.size_threshold
                || o.schedule != n.schedule
                || o.min_file_size != n.min_file_size
                || o.check_interval != n.check_interval
                || o.delete_original_file != n.delete_original_file
                || o.single_nzb_per_folder != n.single_nzb_per_folder
                || o.follow_symlinks != n.follow_symlinks
                || o.min_file_age != n.min_file_age
                || o.min_file_age_to_delete != n.min_file_age_to_delete
                || o.ignore_patterns.clone().unwrap_or_default()
                    != n.ignore_patterns.clone().unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;

    fn cfg(servers: Vec<ServerConfig>) -> ConfigData {
        ConfigData {
            servers: Some(servers),
            watchers: Some(vec![WatcherConfig::default()]),
            ..Default::default()
        }
    }

    fn srv(host: &str, port: i64) -> ServerConfig {
        ServerConfig {
            host: host.into(),
            port,
            ..Default::default()
        }
    }

    #[test]
    fn pool_changes_ignore_order_and_non_provider_fields() {
        let a = cfg(vec![srv("a", 1), srv("b", 2)]);
        assert!(pool_config_changed(None, &a));
        assert!(
            !pool_config_changed(Some(&a), &cfg(vec![srv("b", 2), srv("a", 1)])),
            "reordering is no change"
        );
        let mut renamed = cfg(vec![srv("a", 1), srv("b", 2)]);
        renamed.servers.as_mut().unwrap()[0].name = "pretty".into();
        assert!(!pool_config_changed(Some(&a), &renamed));
        assert!(pool_config_changed(Some(&a), &cfg(vec![srv("a", 1)])));
        assert!(pool_config_changed(
            Some(&a),
            &cfg(vec![srv("a", 1), srv("c", 3)])
        ));
        let mut tweaked = a.clone();
        tweaked.servers.as_mut().unwrap()[1].max_connections = 20;
        assert!(pool_config_changed(Some(&a), &tweaked));
        let mut posting = a.clone();
        posting.posting.max_retries = 9;
        assert!(!pool_config_changed(Some(&a), &posting));
    }

    #[test]
    fn processor_changes_follow_the_captured_fields() {
        let base = cfg(vec![]);
        assert!(processor_config_changed(None, &base));
        assert!(!processor_config_changed(Some(&base), &base.clone()));
        let mut c = base.clone();
        c.output_dir = "/out".into();
        assert!(processor_config_changed(Some(&base), &c));
        let mut c = base.clone();
        c.queue.max_concurrent_uploads = 5;
        assert!(processor_config_changed(Some(&base), &c));
        let mut c = base.clone();
        c.par2.redundancy = "20".into();
        assert!(processor_config_changed(Some(&base), &c) && par2_config_changed(Some(&base), &c));
        let mut c = base.clone();
        c.par2.parpar_extra_args = Some(vec!["-x".into()]);
        assert!(par2_config_changed(Some(&base), &c));
        let mut c = base.clone();
        c.posting.max_retries = 9;
        assert!(
            !processor_config_changed(Some(&base), &c),
            "posting settings are read per job"
        );
    }

    #[test]
    fn watcher_changes_cover_every_field_and_the_ignore_list() {
        let base = cfg(vec![]);
        assert!(watcher_config_changed(None, &base));
        assert!(!watcher_config_changed(Some(&base), &base.clone()));
        let mut c = base.clone();
        c.watchers.as_mut().unwrap()[0].ignore_patterns = Some(vec!["*.tmp".into()]);
        assert!(watcher_config_changed(Some(&base), &c));
        let mut c = base.clone();
        c.watchers.as_mut().unwrap()[0].single_nzb_per_folder = true;
        assert!(watcher_config_changed(Some(&base), &c));
        let mut c = base.clone();
        c.watchers.as_mut().unwrap().push(WatcherConfig::default());
        assert!(watcher_config_changed(Some(&base), &c));
    }
}
