//! Port of `internal/pool`: owns the NNTP connection pools for the app's lifetime — an upload pool
//! (upload-role servers) and, when verify-role servers exist, a dedicated verification pool (otherwise
//! verification shares the upload pool). A settings change updates the live pools incrementally
//! (add/remove only the providers whose connection-relevant settings changed).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use postie_nntp::pool::{Client, ProviderConfig};
use postie_nntp::{Endpoint, Socks5};

use crate::config::{ConfigData, ServerConfig};
use crate::poster::ArticleClient;
use crate::processor::availability::ProviderHealth;
use crate::processor::runner::PoolManager;

/// Canonical provider key (`host:port` or `host:port+user`), matching the NNTP crate's provider names.
pub fn provider_key(s: &ServerConfig) -> String {
    if s.username.is_empty() {
        format!("{}:{}", s.host, s.port)
    } else {
        format!("{}:{}+{}", s.host, s.port, s.username)
    }
}

/// True when any pool-affecting field differs (the predicate behind "re-add this provider").
pub fn server_config_changed(a: &ServerConfig, b: &ServerConfig) -> bool {
    a.host != b.host
        || a.port != b.port
        || a.username != b.username
        || a.password != b.password
        || a.ssl != b.ssl
        || a.insecure_ssl != b.insecure_ssl
        || a.max_connections != b.max_connections
        || a.inflight != b.inflight
        || a.max_connection_idle_time_in_seconds != b.max_connection_idle_time_in_seconds
        || a.max_connection_ttl_in_seconds != b.max_connection_ttl_in_seconds
        || a.proxy_url != b.proxy_url
        || a.is_enabled() != b.is_enabled()
        || a.role != b.role
}

/// `ServerConfigToProvider`: defaults of 10 connections, 300 s idle, 3600 s TTL and inflight 10.
pub fn provider_config(s: &ServerConfig) -> ProviderConfig {
    let params = s.provider_params();
    let mut endpoint = Endpoint::new(&s.host, u16::try_from(s.port).unwrap_or(0));
    endpoint.tls = s.ssl;
    endpoint.insecure_tls = s.insecure_ssl;
    endpoint.username.clone_from(&s.username);
    endpoint.password.clone_from(&s.password);
    endpoint.proxy = (!s.proxy_url.is_empty())
        .then(|| Socks5::parse(&s.proxy_url))
        .flatten();
    let t = crate::timeouts::current();
    endpoint.connect_timeout = t.connect;
    let mut c = ProviderConfig::new(endpoint);
    c.stall_timeout = t.stall;
    c.connections = usize::try_from(params.connections).unwrap_or(10);
    c.inflight = usize::try_from(params.inflight).unwrap_or(10);
    c.idle_timeout = Duration::from_secs(u64::try_from(params.idle_timeout_secs).unwrap_or(300));
    c.ttl = Duration::from_secs(u64::try_from(params.keep_alive_secs).unwrap_or(3600));
    c
}

fn build_client(servers: &[&ServerConfig]) -> Client {
    Client::from_configs(servers.iter().map(|s| provider_config(s)).collect(), 0)
}

struct State {
    upload: Option<Arc<Client>>,
    /// Dedicated verification pool; `None` means verification uses the upload pool.
    verify: Option<Arc<Client>>,
    config: ConfigData,
    closed: bool,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Manager")
    }
}

pub struct Manager {
    state: RwLock<State>,
}

fn diff_providers(client: &Client, old: &[&ServerConfig], new: &[&ServerConfig], pool: &str) {
    let old_map: HashMap<String, &ServerConfig> =
        old.iter().map(|s| (provider_key(s), *s)).collect();
    let new_map: HashMap<String, &ServerConfig> =
        new.iter().map(|s| (provider_key(s), *s)).collect();
    for (key, o) in &old_map {
        if new_map.get(key).is_none_or(|n| server_config_changed(o, n)) {
            if client.remove_provider(key) {
                crate::logx::info(
                    &format!("Removed provider from pool: pool={pool} provider={key}"),
                    &[("component", "pool".into())],
                );
            } else {
                crate::logx::warn(
                    &format!("Failed to remove provider from pool: pool={pool} provider={key}"),
                    &[("component", "pool".into())],
                );
            }
        }
    }
    for (key, n) in &new_map {
        if old_map.get(key).is_none_or(|o| server_config_changed(o, n)) {
            client.add_provider(provider_config(n));
            crate::logx::info(
                &format!("Added provider to pool: pool={pool} provider={key}"),
                &[("component", "pool".into())],
            );
        }
    }
}

impl Manager {
    /// Creates the pools; fails when no upload server is configured. Must run inside a Tokio runtime
    /// (connections are made lazily by background tasks).
    pub fn new(cfg: &ConfigData) -> Result<Self, String> {
        let upload_servers = cfg.upload_servers();
        if upload_servers.is_empty() {
            return Err("failed to create upload pool: no upload servers configured".into());
        }
        let upload = Arc::new(build_client(&upload_servers));
        let verify_servers = cfg.verify_servers();
        let verify = (!verify_servers.is_empty()).then(|| Arc::new(build_client(&verify_servers)));
        crate::logx::info(
            &format!(
                "NNTP connection pools configured: upload_servers={} verify_servers={} using_dedicated_verify_pool={}",
                upload_servers.len(),
                verify_servers.len(),
                verify.is_some()
            ),
            &[("component", "pool".into())],
        );
        Ok(Self {
            state: RwLock::new(State {
                upload: Some(upload),
                verify,
                config: cfg.clone(),
                closed: false,
            }),
        })
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The pool for posting articles.
    pub fn upload_pool(&self) -> Option<Arc<Client>> {
        self.read().upload.clone()
    }

    /// The pool for article verification: verify-role servers when configured, else the upload pool.
    pub fn verify_pool(&self) -> Option<Arc<Client>> {
        let s = self.read();
        s.verify.clone().or_else(|| s.upload.clone())
    }

    /// Applies a new configuration to the live pools without dropping connections that did not change.
    pub fn update_config(&self, new: &ConfigData) -> Result<(), String> {
        let mut s = self.state.write().unwrap_or_else(|e| e.into_inner());
        if s.closed {
            return Err("connection pool manager has been closed".into());
        }
        crate::logx::info(
            "Updating NNTP connection pools with new configuration",
            &[("component", "pool".into())],
        );
        let new_upload = new.upload_servers();
        if s.upload.is_none() {
            if new_upload.is_empty() {
                return Err("failed to create upload pool: no upload servers configured".into());
            }
            s.upload = Some(Arc::new(build_client(&new_upload)));
            let nv = new.verify_servers();
            s.verify = (!nv.is_empty()).then(|| Arc::new(build_client(&nv)));
            s.config = new.clone();
            return Ok(());
        }
        let old_cfg = s.config.clone();
        if let Some(up) = &s.upload {
            diff_providers(up, &old_cfg.upload_servers(), &new_upload, "upload");
        }
        let (old_v, new_v) = (old_cfg.verify_servers(), new.verify_servers());
        match (new_v.is_empty(), old_v.is_empty()) {
            (false, false) if s.verify.is_some() => {
                if let Some(v) = &s.verify {
                    diff_providers(v, &old_v, &new_v, "verify");
                }
            }
            (false, true) => s.verify = Some(Arc::new(build_client(&new_v))),
            (true, false) => {
                if let Some(v) = s.verify.take() {
                    v.close();
                }
            }
            _ => s.verify = None,
        }
        s.config = new.clone();
        crate::logx::info(
            &format!(
                "NNTP connection pools updated successfully: upload_servers={} verify_servers={}",
                new_upload.len(),
                new_v.len()
            ),
            &[("component", "pool".into())],
        );
        Ok(())
    }

    /// Per-provider counters of the upload pool.
    pub fn metrics(&self) -> Result<Vec<postie_nntp::pool::StatsSnapshot>, String> {
        let s = self.read();
        if s.closed {
            return Err("connection pool manager has been closed".into());
        }
        s.upload
            .as_ref()
            .map(|u| u.stats())
            .ok_or_else(|| "connection pool is not available".into())
    }

    /// Gracefully shuts down both pools; idempotent.
    pub fn close(&self) {
        let mut s = self.state.write().unwrap_or_else(|e| e.into_inner());
        if s.closed {
            return;
        }
        if let Some(v) = s.verify.take() {
            v.close();
        }
        if let Some(u) = s.upload.take() {
            u.close();
        }
        s.closed = true;
        crate::logx::info(
            "NNTP connection pool manager closed successfully",
            &[("component", "pool".into())],
        );
    }

    pub fn is_closed(&self) -> bool {
        self.read().closed
    }
}

impl PoolManager for Manager {
    fn upload_client(&self) -> Option<Arc<dyn ArticleClient>> {
        self.upload_pool().map(|c| c as Arc<dyn ArticleClient>)
    }

    fn verify_client(&self) -> Option<Arc<dyn ArticleClient>> {
        self.read()
            .verify
            .clone()
            .map(|c| c as Arc<dyn ArticleClient>)
    }

    fn provider_health(&self) -> Result<Vec<ProviderHealth>, String> {
        Ok(self
            .metrics()?
            .into_iter()
            .map(|s| ProviderHealth {
                name: s.name,
                active_connections: s.active_connections as i64,
                errors: s.errors as i64,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, port: i64, role: &str) -> ServerConfig {
        ServerConfig {
            host: host.into(),
            port,
            role: role.into(),
            max_connections: 3,
            ..Default::default()
        }
    }

    fn cfg(servers: Vec<ServerConfig>) -> ConfigData {
        ConfigData {
            servers: Some(servers),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn keys_and_change_detection_match_go() {
        let mut s = server("h", 119, "upload");
        assert_eq!(provider_key(&s), "h:119");
        s.username = "u".into();
        assert_eq!(provider_key(&s), "h:119+u");
        let base = server("h", 119, "upload");
        assert!(!server_config_changed(&base, &base.clone()));
        for change in [
            |s: &mut ServerConfig| s.host = "x".into(),
            |s: &mut ServerConfig| s.port = 563,
            |s: &mut ServerConfig| s.password = "p".into(),
            |s: &mut ServerConfig| s.ssl = true,
            |s: &mut ServerConfig| s.insecure_ssl = true,
            |s: &mut ServerConfig| s.max_connections = 9,
            |s: &mut ServerConfig| s.inflight = 5,
            |s: &mut ServerConfig| s.max_connection_idle_time_in_seconds = 1,
            |s: &mut ServerConfig| s.max_connection_ttl_in_seconds = 1,
            |s: &mut ServerConfig| s.proxy_url = "socks5://p:1".into(),
            |s: &mut ServerConfig| s.enabled = Some(false),
            |s: &mut ServerConfig| s.role = "verify".into(),
        ] {
            let mut b = base.clone();
            change(&mut b);
            assert!(server_config_changed(&base, &b));
        }
        let mut same = base.clone();
        same.enabled = Some(true);
        same.name = "renamed".into();
        assert!(
            !server_config_changed(&base, &same),
            "an explicit enabled and a cosmetic name do not matter"
        );
    }

    #[test]
    fn provider_config_applies_defaults_and_ssl_proxy_settings() {
        let mut s = server("news.example", 563, "upload");
        s.max_connections = 0;
        s.ssl = true;
        s.insecure_ssl = true;
        s.username = "u".into();
        s.password = "p".into();
        s.proxy_url = "socks5://px:1080".into();
        let c = provider_config(&s);
        assert_eq!(
            (
                c.connections,
                c.inflight,
                c.idle_timeout.as_secs(),
                c.ttl.as_secs()
            ),
            (10, 10, 300, 3600)
        );
        assert!(c.endpoint.tls && c.endpoint.insecure_tls && c.endpoint.proxy.is_some());
        assert_eq!(
            (
                c.endpoint.host.as_str(),
                c.endpoint.port,
                c.endpoint.username.as_str()
            ),
            ("news.example", 563, "u")
        );
    }

    #[tokio::test]
    async fn needs_an_upload_server_and_shares_the_pool_without_verify_servers() {
        assert!(
            Manager::new(&cfg(vec![]))
                .unwrap_err()
                .contains("no upload servers configured")
        );
        assert!(Manager::new(&cfg(vec![server("h", 1, "verify")])).is_err());
        let m = Manager::new(&cfg(vec![server("a", 1, "upload")])).unwrap();
        assert!(Arc::ptr_eq(
            &m.upload_pool().unwrap(),
            &m.verify_pool().unwrap()
        ));
        assert!(m.verify_client().is_none(), "no dedicated verify pool");
        let dedicated = Manager::new(&cfg(vec![
            server("a", 1, "upload"),
            server("v", 2, "verify"),
        ]))
        .unwrap();
        assert!(!Arc::ptr_eq(
            &dedicated.upload_pool().unwrap(),
            &dedicated.verify_pool().unwrap()
        ));
        assert!(dedicated.verify_client().is_some());
    }

    #[tokio::test]
    async fn update_config_changes_providers_incrementally() {
        let m = Manager::new(&cfg(vec![
            server("a", 1, "upload"),
            server("b", 2, "upload"),
        ]))
        .unwrap();
        let before = m.upload_pool().unwrap();
        let mut b2 = server("b", 2, "upload");
        b2.max_connections = 7;
        m.update_config(&cfg(vec![
            server("a", 1, "upload"),
            b2,
            server("c", 3, "upload"),
        ]))
        .unwrap();
        let after = m.upload_pool().unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "the same client object is updated in place"
        );
        let mut names: Vec<String> = after.stats().into_iter().map(|s| s.name).collect();
        names.sort();
        assert_eq!(names, ["a:1", "b:2", "c:3"]);
        assert_eq!(after.max_connections(), 3 + 7 + 3);
        // verify pool transitions: none -> dedicated -> none
        m.update_config(&cfg(vec![
            server("a", 1, "upload"),
            server("v", 9, "verify"),
        ]))
        .unwrap();
        assert!(m.verify_client().is_some());
        m.update_config(&cfg(vec![server("a", 1, "upload")]))
            .unwrap();
        assert!(m.verify_client().is_none());
        assert_eq!(m.provider_health().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn close_is_idempotent_and_blocks_further_use() {
        let m = Manager::new(&cfg(vec![server("a", 1, "upload")])).unwrap();
        assert!(!m.is_closed());
        m.close();
        m.close();
        assert!(m.is_closed() && m.upload_pool().is_none());
        assert!(m.metrics().unwrap_err().contains("closed"));
        assert!(
            m.update_config(&cfg(vec![server("a", 1, "upload")]))
                .unwrap_err()
                .contains("closed")
        );
        assert!(m.provider_health().is_err());
    }
}
