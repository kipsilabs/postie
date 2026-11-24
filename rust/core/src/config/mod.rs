pub mod address;
pub mod duration;

use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const DEFAULT_REDUNDANCY: &str = "5%";
pub const CURRENT_CONFIG_VERSION: i64 = 2;

pub const SERVER_ROLE_UPLOAD: &str = "upload";
pub const SERVER_ROLE_VERIFY: &str = "verify";
pub const PAR2_GF16_METHOD_AUTO: &str = "auto";

/// Accepted values for `par2.gf16_method`.
pub const PAR2_GF16_METHODS: &[&str] = &[
    PAR2_GF16_METHOD_AUTO,
    "lookup",
    "lookup3",
    "shuffle-avx2",
    "shuffle-avx512",
    "shuffle-vbmi",
    "xor-jit-avx2",
    "affine-avx2",
    "affine-avx512",
    "shuffle-neon",
    "clmul-neon",
];

/// yaml.v3 decodes `y/yes/on` and `n/no/off` (any of the usual cases) into a bool field.
fn bool_from_yaml_word(s: &str) -> Option<bool> {
    match s {
        "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" | "true" | "True" | "TRUE" => {
            Some(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" | "false" | "False" | "FALSE" => {
            Some(false)
        }
        _ => None,
    }
}

fn lenient_opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum B {
        Bool(bool),
        Word(String),
    }
    match Option::<B>::deserialize(d)? {
        None => Ok(None),
        Some(B::Bool(b)) => Ok(Some(b)),
        Some(B::Word(w)) => bool_from_yaml_word(&w)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("cannot unmarshal {w:?} into bool"))),
    }
}

fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(lenient_opt_bool(d)?.unwrap_or(false))
}

/// A duration stored as the string Go would print. Deserializing validates with Go's
/// `time.ParseDuration` and normalizes (`"5m"` becomes `"5m0s"`); an empty string stays empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Duration(pub String);

impl Duration {
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Nanoseconds; 0 when unparseable (Go: `ToDuration` ignores the error).
    pub fn nanos(&self) -> i64 {
        duration::parse_duration(&self.0).unwrap_or(0)
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty() {
            return Ok(Self(String::new()));
        }
        let ns = duration::parse_duration(&s).map_err(serde::de::Error::custom)?;
        Ok(Self(duration::format_duration(ns)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionPoolConfig {
    pub min_connections: i64,
    pub health_check_interval: Duration,
}

/// Optional generic pre-upload NZB lookup.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplicateLookupConfig {
    #[serde(deserialize_with = "lenient_bool")]
    pub enabled: bool,
    pub url: String,
    pub api_key: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Par2Config {
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub enabled: Option<bool>,
    pub redundancy: String,
    pub temp_dir: String,
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub maintain_par2_files: Option<bool>,
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub skip_if_par2_exists: Option<bool>,
    pub parpar_binary_path: String,
    pub parpar_extra_args: Option<Vec<String>>,
    pub num_goroutines: i64,
    pub memory_limit: i64,
    pub slice_size: i64,
    /// Caps how many PAR2 operations run at once process-wide. Default 1.
    pub max_concurrent_jobs: i64,
    /// Pins the GF16 SIMD kernel; `auto` picks a safe one for the CPU.
    pub gf16_method: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub password: String,
    #[serde(deserialize_with = "lenient_bool")]
    pub ssl: bool,
    pub max_connections: i64,
    pub max_connection_idle_time_in_seconds: i64,
    pub max_connection_ttl_in_seconds: i64,
    #[serde(deserialize_with = "lenient_bool")]
    pub insecure_ssl: bool,
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub enabled: Option<bool>,
    /// "upload" for posting, "verify" for STAT checks only.
    pub role: String,
    /// Deprecated (v1 configs): use `role`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub check_only: Option<bool>,
    /// Concurrent requests per connection (0 means the pool default).
    pub inflight: i64,
    /// SOCKS5 proxy, `socks5://username:password@hostname:port`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PostHeaders {
    #[serde(deserialize_with = "lenient_bool")]
    pub add_nxg_header: bool,
    pub default_from: String,
    pub custom_headers: Option<Vec<CustomHeader>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PostCheck {
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub enabled: Option<bool>,
    #[serde(rename = "delay")]
    pub retry_delay: Duration,
    #[serde(rename = "max_reposts")]
    pub max_repost: u64,
    pub deferred_check_delay: Duration,
    pub deferred_max_retries: i64,
    pub deferred_max_backoff: Duration,
    pub deferred_check_interval: Duration,
    pub deferred_batch_size: i64,
    pub stat_batch_size: i64,
    /// 0 enables automatic sizing.
    pub max_concurrent_checks: i64,
    /// STAT each article shortly after it is posted, while the rest of the upload runs.
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub check_during_upload: Option<bool>,
    /// Upload connections set aside for those checks when verification shares the upload servers
    /// (no verify-role server). 0 = auto (see `postie::stater::reserved_check_connections`).
    pub check_connections: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewsgroupConfig {
    pub name: String,
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PostingConfig {
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub wait_for_par2: Option<bool>,
    pub max_retries: i64,
    pub retry_delay: Duration,
    pub article_size_in_bytes: u64,
    pub groups: Option<Vec<NewsgroupConfig>>,
    /// Bytes per second; 0 is unlimited.
    pub throttle_rate: i64,
    pub message_id_format: String,
    pub post_headers: PostHeaders,
    pub obfuscation_policy: String,
    pub par2_obfuscation_policy: String,
    pub group_policy: String,
    /// 0 enables automatic sizing.
    pub upload_buffer_memory_limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    pub start_time: String,
    pub end_time: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WatcherConfig {
    pub name: String,
    #[serde(deserialize_with = "lenient_bool")]
    pub enabled: bool,
    pub watch_directory: String,
    pub size_threshold: i64,
    pub schedule: ScheduleConfig,
    pub ignore_patterns: Option<Vec<String>>,
    pub min_file_size: i64,
    pub check_interval: Duration,
    #[serde(deserialize_with = "lenient_bool")]
    pub delete_original_file: bool,
    #[serde(deserialize_with = "lenient_bool")]
    pub single_nzb_per_folder: bool,
    #[serde(deserialize_with = "lenient_bool")]
    pub follow_symlinks: bool,
    pub min_file_age: Duration,
    pub min_file_age_to_delete: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NzbCompressionConfig {
    #[serde(deserialize_with = "lenient_bool")]
    pub enabled: bool,
    #[serde(rename = "type")]
    pub kind: String,
    pub level: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub database_type: String,
    pub database_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueConfig {
    pub max_concurrent_uploads: i64,
    /// Holds back queue processing until pending file jobs reach this many bytes (0 disables).
    pub min_size_to_start: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    #[serde(deserialize_with = "lenient_bool")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PostUploadScriptConfig {
    #[serde(deserialize_with = "lenient_bool")]
    pub enabled: bool,
    pub command: String,
    pub timeout: Duration,
    pub max_retries: i64,
    pub retry_delay: Duration,
    pub max_backoff: Duration,
    pub max_retry_duration: Duration,
    pub retry_check_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArrInstance {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    pub api_key: String,
    #[serde(deserialize_with = "lenient_bool")]
    pub enabled: bool,
    pub webhook_id: i64,
    #[serde(deserialize_with = "lenient_bool")]
    pub delete_after_upload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArrConfig {
    pub instances: Option<Vec<ArrInstance>>,
}

/// The persisted application configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigData {
    pub version: i64,
    pub servers: Option<Vec<ServerConfig>>,
    pub connection_pool: ConnectionPoolConfig,
    pub posting: PostingConfig,
    pub post_check: PostCheck,
    pub par2: Par2Config,
    /// Deprecated: use `watchers`.
    pub watcher: WatcherConfig,
    pub watchers: Option<Vec<WatcherConfig>>,
    pub nzb_compression: NzbCompressionConfig,
    pub database: DatabaseConfig,
    pub queue: QueueConfig,
    pub api: ApiConfig,
    pub output_dir: String,
    #[serde(deserialize_with = "lenient_opt_bool")]
    pub maintain_original_extension: Option<bool>,
    pub post_upload_script: PostUploadScriptConfig,
    pub arr: ArrConfig,
    pub duplicate_lookup: DuplicateLookupConfig,
}

/// Progress snapshot for file processing (camelCase JSON, as the Go struct tags).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProgressStatus {
    pub current_file: String,
    pub total_files: i64,
    pub completed_files: i64,
    pub stage: String,
    pub details: String,
    pub is_running: bool,
    pub last_update: i64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub percentage: f64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub current_file_progress: f64,
    #[serde(rename = "jobID")]
    pub job_id: String,
    pub total_bytes: i64,
    pub transferred_bytes: i64,
    pub current_file_bytes: i64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub speed: f64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub seconds_left: f64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub elapsed_time: f64,
}

/// Errors carry the Go messages.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("error reading config file: {0}")]
    Read(String),
    #[error("error parsing config file: {0}")]
    Parse(String),
    #[error(
        "config version {0} is newer than supported version {CURRENT_CONFIG_VERSION}; please upgrade the application"
    )]
    TooNew(i64),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("invalid configuration format: {0}")]
    Format(String),
    #[error("{0}")]
    Write(std::io::Error),
}

pub fn is_config_version_too_new(version: i64) -> bool {
    version > CURRENT_CONFIG_VERSION
}

/// Go leaves absent or `null` fields at their zero values, so nulls are dropped before typing.
fn strip_nulls(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|_, x| !x.is_null());
            m.values_mut().for_each(strip_nulls);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// yaml.v3 decodes an integral float (`563.0`) into an integer field.
fn integral_floats_to_ints(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(f) = n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e18) {
                *v = Value::from(f as i64);
            }
        }
        Value::Object(m) => m.values_mut().for_each(integral_floats_to_ints),
        Value::Array(a) => a.iter_mut().for_each(integral_floats_to_ints),
        _ => {}
    }
}

impl ConfigData {
    /// Parses YAML like `yaml.Unmarshal` into the config (no defaults applied).
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let yaml: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
        let mut v = serde_json::to_value(&yaml).map_err(|e| e.to_string())?;
        if v.is_null() {
            return Ok(Self::default());
        }
        strip_nulls(&mut v);
        integral_floats_to_ints(&mut v);
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    /// Parses the JSON the frontend sends (same keys as the YAML).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let mut v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        strip_nulls(&mut v);
        serde_json::from_value(v).map_err(|e| e.to_string())
    }
}

fn some_bool(v: bool) -> Option<bool> {
    Some(v)
}

fn default_watcher() -> WatcherConfig {
    WatcherConfig {
        name: String::new(),
        enabled: false,
        watch_directory: String::new(),
        size_threshold: 104_857_600, // 100MB
        schedule: ScheduleConfig {
            start_time: "00:00".into(),
            end_time: "23:59".into(),
        },
        ignore_patterns: Some(vec!["*.tmp".into(), "*.part".into(), "*.!ut".into()]),
        min_file_size: 1_048_576, // 1MB
        check_interval: Duration::new("5m"),
        delete_original_file: false,
        single_nzb_per_folder: false,
        follow_symlinks: false,
        min_file_age: Duration::new("60s"),
        min_file_age_to_delete: Duration::new("0s"),
    }
}

/// A default configuration (`GetDefaultConfig`).
pub fn default_config() -> ConfigData {
    ConfigData {
        version: CURRENT_CONFIG_VERSION,
        servers: Some(vec![]),
        connection_pool: ConnectionPoolConfig {
            min_connections: 0,
            health_check_interval: Duration::new("1m"),
        },
        posting: PostingConfig {
            wait_for_par2: some_bool(false),
            max_retries: 3,
            retry_delay: Duration::new("5s"),
            article_size_in_bytes: 768_000, // 768KB
            groups: Some(vec![NewsgroupConfig {
                name: "alt.binaries.test".into(),
                enabled: some_bool(true),
            }]),
            throttle_rate: 0,
            message_id_format: "random".into(),
            post_headers: PostHeaders {
                add_nxg_header: false,
                default_from: String::new(),
                custom_headers: Some(vec![]),
            },
            obfuscation_policy: "full".into(),
            par2_obfuscation_policy: "full".into(),
            group_policy: "each_file".into(),
            upload_buffer_memory_limit: 0,
        },
        post_check: PostCheck {
            enabled: some_bool(true),
            retry_delay: Duration::new("10s"),
            max_repost: 1,
            deferred_check_delay: Duration::new("30s"),
            deferred_max_retries: 5,
            deferred_max_backoff: Duration::new("5m"),
            deferred_check_interval: Duration::new("2m"),
            deferred_batch_size: 10_000,
            stat_batch_size: 100,
            max_concurrent_checks: 0,
            check_during_upload: some_bool(true),
            check_connections: 0,
        },
        par2: Par2Config {
            enabled: some_bool(true),
            redundancy: DEFAULT_REDUNDANCY.into(),
            temp_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            maintain_par2_files: some_bool(false),
            memory_limit: 4 * 1024 * 1024 * 1024,
            slice_size: 10 * 1024 * 1024,
            max_concurrent_jobs: 1,
            gf16_method: PAR2_GF16_METHOD_AUTO.into(),
            ..Default::default()
        },
        watcher: WatcherConfig::default(),
        watchers: Some(vec![default_watcher()]),
        nzb_compression: NzbCompressionConfig {
            enabled: false,
            kind: "none".into(),
            level: 0,
        },
        database: DatabaseConfig {
            database_type: "sqlite".into(),
            database_path: "./postie.db".into(),
        },
        queue: QueueConfig {
            max_concurrent_uploads: 1,
            min_size_to_start: 0,
        },
        api: ApiConfig::default(),
        output_dir: "./output".into(),
        maintain_original_extension: some_bool(true),
        post_upload_script: PostUploadScriptConfig {
            enabled: false,
            command: String::new(),
            timeout: Duration::new("30s"),
            max_retries: 3,
            retry_delay: Duration::new("30s"),
            max_backoff: Duration::new("1h"),
            max_retry_duration: Duration::new("24h"),
            retry_check_interval: Duration::new("1m"),
        },
        arr: ArrConfig::default(),
        duplicate_lookup: DuplicateLookupConfig::default(),
    }
}

/// Connection parameters derived from a server entry, with the defaults the pool applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderParams {
    pub address: String,
    pub connections: i64,
    pub inflight: i64,
    pub idle_timeout_secs: i64,
    pub keep_alive_secs: i64,
}

impl ServerConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// `ServerConfigToProvider` defaults: 10 connections, 300 s idle, 3600 s TTL, inflight 10.
    pub fn provider_params(&self) -> ProviderParams {
        let pos = |v: i64, d: i64| if v <= 0 { d } else { v };
        ProviderParams {
            address: format!("{}:{}", self.host, self.port),
            connections: pos(self.max_connections, 10),
            inflight: pos(self.inflight, 10),
            idle_timeout_secs: pos(self.max_connection_idle_time_in_seconds, 300),
            keep_alive_secs: pos(self.max_connection_ttl_in_seconds, 3600),
        }
    }
}

/// Whether an `http(s)` URL has a host and no embedded credentials (Go: `url.Parse` rules).
fn is_plain_http_url(s: &str) -> bool {
    let Some((_, rest)) = s.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    reqwest::Url::parse(s).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
    })
}

impl ConfigData {
    /// `GetDuplicateLookupConfig`: the timeout defaults to 60 s.
    pub fn duplicate_lookup_config(&self) -> DuplicateLookupConfig {
        let mut c = self.duplicate_lookup.clone();
        if c.timeout.is_empty() {
            c.timeout = Duration::new("60s");
        }
        c
    }

    /// Enabled servers that post articles (role is not `verify`).
    pub fn upload_servers(&self) -> Vec<&ServerConfig> {
        self.servers
            .iter()
            .flatten()
            .filter(|s| s.is_enabled() && s.role != SERVER_ROLE_VERIFY)
            .collect()
    }

    /// Enabled servers used only for STAT checks.
    pub fn verify_servers(&self) -> Vec<&ServerConfig> {
        self.servers
            .iter()
            .flatten()
            .filter(|s| s.is_enabled() && s.role == SERVER_ROLE_VERIFY)
            .collect()
    }

    /// Every enabled server (the all-purpose pool).
    pub fn enabled_servers(&self) -> Vec<&ServerConfig> {
        self.servers
            .iter()
            .flatten()
            .filter(|s| s.is_enabled())
            .collect()
    }

    /// First watcher (legacy single-watcher accessor).
    pub fn watcher_config(&self) -> WatcherConfig {
        match self.watchers.as_deref() {
            Some([first, ..]) => first.clone(),
            _ => self.watcher.clone(),
        }
    }

    pub fn output_dir(&self) -> &str {
        if self.output_dir.is_empty() {
            "./output"
        } else {
            &self.output_dir
        }
    }

    pub fn maintain_original_extension(&self) -> bool {
        self.maintain_original_extension.unwrap_or(true)
    }

    fn migrate_watcher_config(&mut self) {
        if self.watchers.as_ref().is_none_or(Vec::is_empty)
            && !self.watcher.watch_directory.is_empty()
        {
            self.watchers = Some(vec![std::mem::take(&mut self.watcher)]);
        }
        if self.watchers.as_ref().is_none_or(Vec::is_empty) {
            self.watchers = Some(vec![default_watcher()]);
        }
    }

    /// Validates the configuration; the error is Go's message (without the `invalid configuration: `
    /// prefix that `load` adds).
    pub fn validate(&self) -> Result<(), String> {
        let dl = self.duplicate_lookup_config();
        if dl.enabled {
            if !is_plain_http_url(&dl.url) {
                return Err("duplicate lookup URL must be an HTTP or HTTPS endpoint without embedded credentials".into());
            }
            let ns = duration::parse_duration(&dl.timeout.0).unwrap_or(0);
            if duration::parse_duration(&dl.timeout.0).is_err() || ns <= 0 || ns > 300_000_000_000 {
                return Err("duplicate lookup timeout must be positive and at most 5m".into());
            }
        }
        let servers = self.servers.as_deref().unwrap_or(&[]);
        for (i, s) in servers.iter().enumerate() {
            if s.host.is_empty() {
                return Err(format!("server {i}: host is required"));
            }
            if s.port <= 0 || s.port > 65535 {
                return Err(format!("server {i}: invalid port number"));
            }
            if s.max_connections <= 0 {
                return Err(format!("server {i}: max_connections must be positive"));
            }
        }
        let mut seen: std::collections::HashMap<(&str, &str), usize> =
            std::collections::HashMap::new();
        for (i, s) in servers.iter().enumerate() {
            if let Some(&j) = seen.get(&(s.host.as_str(), s.username.as_str())) {
                return Err(format!(
                    "servers {} and {} have the same host ({:?}) and username ({:?}); each provider account must be unique",
                    j + 1,
                    i + 1,
                    s.host,
                    s.username
                ));
            }
            seen.insert((s.host.as_str(), s.username.as_str()), i);
        }
        if self.upload_servers().is_empty() {
            return Err("at least one upload server is required".into());
        }
        if self.posting.groups.as_ref().is_none_or(Vec::is_empty) {
            return Err("posting groups are required".into());
        }
        let c = &self.nzb_compression;
        if c.enabled {
            match c.kind.as_str() {
                "zstd" if !(1..=22).contains(&c.level) => {
                    return Err(format!(
                        "invalid zstd compression level: {} (must be between 1-22)",
                        c.level
                    ));
                }
                "brotli" if !(0..=11).contains(&c.level) => {
                    return Err(format!(
                        "invalid brotli compression level: {} (must be between 0-11)",
                        c.level
                    ));
                }
                "zip" if !(0..=9).contains(&c.level) => {
                    return Err(format!(
                        "invalid zip compression level: {} (must be between 0-9)",
                        c.level
                    ));
                }
                "zstd" | "brotli" | "zip" | "none" => {}
                other => return Err(format!("invalid compression type: {other}")),
            }
        }
        match self.database.database_type.as_str() {
            "sqlite" | "postgres" | "mysql" => {}
            other => {
                return Err(format!(
                    "invalid database type: {other} (supported: sqlite, postgres, mysql)"
                ));
            }
        }
        if self.queue.max_concurrent_uploads <= 0 {
            return Err("queue max concurrent uploads must be positive".into());
        }
        if self.posting.upload_buffer_memory_limit < 0 {
            return Err("posting upload_buffer_memory_limit must be >= 0 (0 = auto)".into());
        }
        if self.par2.max_concurrent_jobs < 0 {
            return Err("par2 max_concurrent_jobs must be >= 0 (0 = auto)".into());
        }
        if !self.par2.gf16_method.is_empty()
            && !PAR2_GF16_METHODS.contains(&self.par2.gf16_method.as_str())
        {
            return Err(format!(
                "invalid par2 gf16_method: {:?} (accepted: {})",
                self.par2.gf16_method,
                PAR2_GF16_METHODS.join(", ")
            ));
        }
        if self.post_check.max_concurrent_checks < 0 {
            return Err("post_check max_concurrent_checks must be >= 0 (0 = auto)".into());
        }
        if self.post_check.check_connections < 0 {
            return Err("post_check check_connections must be >= 0 (0 = auto)".into());
        }
        let from = &self.posting.post_headers.default_from;
        if !from.is_empty() {
            address::validate_address(from).map_err(|e| {
                format!("posting post_headers default_from is not a valid email address: {e}")
            })?;
        }
        Ok(())
    }

    /// Applies the load-time defaults and migrations (everything `Load` does after parsing).
    pub fn apply_load_defaults(&mut self) {
        let cfg = self;
        if cfg.posting.max_retries <= 0 {
            cfg.posting.max_retries = 3;
        }
        let set = |d: &mut Duration, v: &str| {
            if d.is_empty() {
                *d = Duration::new(v);
            }
        };
        set(&mut cfg.posting.retry_delay, "5s");
        set(&mut cfg.post_upload_script.timeout, "30s");
        if cfg.post_upload_script.max_retries < 0 {
            cfg.post_upload_script.max_retries = 3; // 0 means unlimited
        }
        set(&mut cfg.post_upload_script.retry_delay, "30s");
        set(&mut cfg.post_upload_script.max_backoff, "1h");
        set(&mut cfg.post_upload_script.max_retry_duration, "24h");
        set(&mut cfg.post_upload_script.retry_check_interval, "1m");
        if cfg.posting.article_size_in_bytes == 0 {
            cfg.posting.article_size_in_bytes = 750_000;
        }
        if cfg.posting.throttle_rate <= 0 {
            cfg.posting.throttle_rate = 0;
        }
        if cfg.posting.group_policy.is_empty() {
            cfg.posting.group_policy = "each_file".into();
        }
        if cfg.posting.message_id_format.is_empty() {
            cfg.posting.message_id_format = "random".into();
        }
        if cfg.posting.obfuscation_policy.is_empty() {
            cfg.posting.obfuscation_policy = "full".into();
        }
        if cfg.par2.enabled.is_none() {
            cfg.par2.enabled = Some(true);
        }
        match cfg.posting.wait_for_par2 {
            // unlike Go: PAR2 overlaps the upload unless the config asks to wait (or has no PAR2)
            None => cfg.posting.wait_for_par2 = Some(cfg.par2.enabled == Some(false)),
            Some(false) if cfg.par2.enabled == Some(false) => {
                cfg.posting.wait_for_par2 = Some(true)
            }
            Some(false) => {} // par2 runs in parallel with the upload
            Some(true) => {}
        }
        if cfg.post_check.enabled.is_none() {
            cfg.post_check.enabled = Some(true);
        }
        set(&mut cfg.post_check.deferred_check_delay, "30s");
        if cfg.post_check.deferred_max_retries == 0 {
            cfg.post_check.deferred_max_retries = 5;
        }
        set(&mut cfg.post_check.deferred_max_backoff, "5m");
        set(&mut cfg.post_check.deferred_check_interval, "2m");
        if cfg.post_check.deferred_batch_size <= 0 {
            cfg.post_check.deferred_batch_size = 10_000;
        }
        if cfg.post_check.stat_batch_size <= 0 {
            cfg.post_check.stat_batch_size = 100;
        }
        if cfg.post_check.check_during_upload.is_none() {
            cfg.post_check.check_during_upload = Some(true);
        }
        if cfg.par2.redundancy.is_empty() {
            cfg.par2.redundancy = DEFAULT_REDUNDANCY.into();
        }
        if cfg.par2.memory_limit == 0 {
            cfg.par2.memory_limit = 4 * 1024 * 1024 * 1024;
        }
        if cfg.par2.slice_size == 0 {
            cfg.par2.slice_size = 10 * 1024 * 1024;
        }
        if cfg.par2.max_concurrent_jobs <= 0 {
            cfg.par2.max_concurrent_jobs = 1;
        }
        if cfg.par2.gf16_method.is_empty() {
            cfg.par2.gf16_method = PAR2_GF16_METHOD_AUTO.into();
        }
        if cfg.par2.maintain_par2_files.is_none() {
            cfg.par2.maintain_par2_files = Some(false);
        }
        if cfg.par2.skip_if_par2_exists.is_none() {
            cfg.par2.skip_if_par2_exists = Some(true);
        }
        if cfg.nzb_compression.kind.is_empty() {
            cfg.nzb_compression.kind = "none".into();
        }
        if cfg.nzb_compression.level == 0 {
            cfg.nzb_compression.level = match cfg.nzb_compression.kind.as_str() {
                "zstd" => 3,
                "brotli" => 4,
                "zip" => 6,
                _ => 0,
            };
        }
        if cfg.database.database_type.is_empty() {
            cfg.database.database_type = "sqlite".into();
        }
        if cfg.database.database_path.is_empty() {
            cfg.database.database_path = "./postie.db".into();
        }
        if cfg.queue.max_concurrent_uploads <= 0 {
            cfg.queue.max_concurrent_uploads = 1;
        }
        if cfg.maintain_original_extension.is_none() {
            cfg.maintain_original_extension = Some(true);
        }
        for s in cfg.servers.iter_mut().flatten() {
            if s.enabled.is_none() {
                s.enabled = Some(true);
            }
            if s.role.is_empty() {
                s.role = SERVER_ROLE_UPLOAD.into();
            }
        }
        cfg.migrate_watcher_config();
    }

    /// v0/v1 migration: `check_only` becomes `role`. Returns whether anything was migrated.
    fn migrate_v1(&mut self) -> bool {
        if self.version > 1 {
            return false;
        }
        for s in self.servers.iter_mut().flatten() {
            if s.role.is_empty() {
                s.role = if s.check_only == Some(true) {
                    SERVER_ROLE_VERIFY
                } else {
                    SERVER_ROLE_UPLOAD
                }
                .into();
            }
            s.check_only = None;
        }
        self.version = CURRENT_CONFIG_VERSION;
        true
    }
}

/// Loads, migrates, defaults and validates the configuration file (`config.Load`).
/// Go's `syscall.Errno` text for the common file errors.
pub fn go_io_text(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        std::io::ErrorKind::AlreadyExists => "file exists".into(),
        _ => e.to_string().to_lowercase(),
    }
}

pub fn load(path: &Path) -> Result<ConfigData, ConfigError> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Read(format!("open {}: {}", path.display(), go_io_text(&e))))?;
    let mut cfg = ConfigData::from_yaml(&data).map_err(ConfigError::Parse)?;
    if is_config_version_too_new(cfg.version) {
        return Err(ConfigError::TooNew(cfg.version));
    }
    if cfg.migrate_v1() {
        // persist so later loads do not migrate again; a failed save is only a warning in Go
        if let Err(e) = save_config(&cfg, path) {
            crate::logx::warn(&format!("Failed to save migrated config to disk: {e}"), &[]);
        }
    }
    cfg.apply_load_defaults();
    cfg.validate().map_err(ConfigError::Invalid)?;
    Ok(cfg)
}

/// Loads from a YAML string (same pipeline as [`load`], minus the file and the v1 re-save).
pub fn load_str(text: &str) -> Result<ConfigData, ConfigError> {
    let mut cfg = ConfigData::from_yaml(text).map_err(ConfigError::Parse)?;
    if is_config_version_too_new(cfg.version) {
        return Err(ConfigError::TooNew(cfg.version));
    }
    cfg.migrate_v1();
    cfg.apply_load_defaults();
    cfg.validate().map_err(ConfigError::Invalid)?;
    Ok(cfg)
}

const SLICE_KEYS: &[&str] = &[
    "servers",
    "groups",
    "custom_headers",
    "parpar_extra_args",
    "ignore_patterns",
    "watchers",
    "instances",
];

fn yaml_output(cfg: &ConfigData) -> Result<serde_yaml_ng::Value, ConfigError> {
    use serde_yaml_ng::Value as Y;
    let mut v = serde_yaml_ng::to_value(cfg).map_err(|e| ConfigError::Format(e.to_string()))?;
    fn walk(v: &mut Y) {
        match v {
            Y::Mapping(m) => {
                for (k, x) in m.iter_mut() {
                    if x.is_null() && k.as_str().is_some_and(|k| SLICE_KEYS.contains(&k)) {
                        *x = Y::Sequence(vec![]); // yaml.v3 writes a nil slice as []
                    }
                    walk(x);
                }
            }
            Y::Sequence(s) => s.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    if let Y::Mapping(m) = &mut v {
        if cfg.watcher == WatcherConfig::default() {
            m.remove("watcher"); // omitempty on a zero struct
        }
        if cfg.arr.instances.as_ref().is_none_or(Vec::is_empty) {
            m.remove("arr");
        }
    }
    Ok(v)
}

/// Writes the configuration as YAML (`config.SaveConfig`).
pub fn save_config(cfg: &ConfigData, path: &Path) -> Result<(), ConfigError> {
    let text = serde_yaml_ng::to_string(&yaml_output(cfg)?)
        .map_err(|e| ConfigError::Format(e.to_string()))?;
    std::fs::write(path, text).map_err(ConfigError::Write)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[derive(serde::Deserialize)]
    struct Case {
        yaml: String,
        ok: bool,
        #[serde(default)]
        config: Value,
        #[serde(default)]
        error: String,
    }

    /// Postie departs from Go here on purpose: a config that does not say otherwise creates PAR2 while
    /// the upload runs instead of before it, so the uplink is not idle during parity creation.
    #[test]
    fn a_config_without_wait_for_par2_overlaps_par2_with_the_upload() {
        const BASE: &str = "servers:\n  - host: news.example.com\n    port: 563\n    max_connections: 10\nposting:\n  groups:\n    - name: alt.binaries.test\n      enabled: true\n";
        assert_eq!(default_config().posting.wait_for_par2, Some(false));
        assert_eq!(load_str(BASE).unwrap().posting.wait_for_par2, Some(false));
        let explicit = format!("{BASE}  wait_for_par2: true\n");
        assert_eq!(
            load_str(&explicit).unwrap().posting.wait_for_par2,
            Some(true),
            "a saved choice is kept"
        );
        let no_par2 = format!("{BASE}par2:\n  enabled: false\n");
        assert_eq!(
            load_str(&no_par2).unwrap().posting.wait_for_par2,
            Some(true),
            "nothing to overlap without PAR2"
        );
    }

    /// Every case recorded from Go's `config.Load` (YAML in, JSON config or error text out).
    #[test]
    fn load_matches_go_oracle() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/config-load.json"
        ))
        .unwrap();
        let cases: std::collections::BTreeMap<String, Case> = serde_json::from_str(&raw).unwrap();
        let mut bad = Vec::new();
        for (name, c) in &cases {
            match (load_str(&c.yaml), c.ok) {
                (Ok(cfg), true) => {
                    let got = serde_json::to_value(&cfg).unwrap();
                    if got != c.config {
                        let diff: Vec<String> = diff_values("", &c.config, &got);
                        bad.push(format!("{name}: config differs: {}", diff.join("; ")));
                    }
                }
                (Ok(_), false) => {
                    bad.push(format!("{name}: Go failed ({}) but Rust loaded", c.error))
                }
                (Err(e), true) => bad.push(format!("{name}: Go loaded but Rust failed: {e}")),
                (Err(e), false) => {
                    let e = e.to_string();
                    let ok = if c.error.starts_with("error parsing config file: ") {
                        e.starts_with("error parsing config file: ")
                    } else {
                        e == c.error
                    };
                    if !ok {
                        bad.push(format!(
                            "{name}: error text\n    go:   {}\n    rust: {e}",
                            c.error.replace('\n', "\\n")
                        ));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "{} of {} cases differ:\n{}",
            bad.len(),
            cases.len(),
            bad.join("\n")
        );
    }

    fn diff_values(path: &str, a: &Value, b: &Value) -> Vec<String> {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                let mut out = Vec::new();
                for k in x
                    .keys()
                    .chain(y.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    match (x.get(k), y.get(k)) {
                        (Some(p), Some(q)) => out.extend(diff_values(&format!("{path}.{k}"), p, q)),
                        (p, q) => out.push(format!("{path}.{k}: go={p:?} rust={q:?}")),
                    }
                }
                out
            }
            _ if a != b => vec![format!("{path}: go={a} rust={b}")],
            _ => vec![],
        }
    }

    #[test]
    fn default_config_matches_go_defaults_and_validates() {
        let mut cfg = default_config();
        assert_eq!(
            (
                cfg.par2.max_concurrent_jobs,
                cfg.posting.upload_buffer_memory_limit,
                cfg.post_check.max_concurrent_checks
            ),
            (1, 0, 0)
        );
        assert_eq!(cfg.par2.gf16_method, "auto");
        cfg.servers = Some(vec![ServerConfig {
            host: "news.example.com".into(),
            port: 563,
            ssl: true,
            max_connections: 10,
            enabled: Some(true),
            role: "upload".into(),
            ..Default::default()
        }]);
        cfg.validate().unwrap();
        let mut c = cfg.clone();
        c.posting.upload_buffer_memory_limit = -1;
        assert!(
            c.validate()
                .unwrap_err()
                .contains("upload_buffer_memory_limit")
        );
        for m in PAR2_GF16_METHODS {
            let mut c = cfg.clone();
            c.par2.gf16_method = (*m).into();
            c.validate().unwrap();
        }
        let mut c = cfg.clone();
        c.par2.gf16_method = "bogus".into();
        let msg = c.validate().unwrap_err();
        assert!(msg.contains("gf16_method") && PAR2_GF16_METHODS.iter().all(|m| msg.contains(m)));
    }

    #[test]
    fn json_roundtrip_and_save_reload_are_stable() {
        let cfg = load_str("servers:\n  - host: h\n    port: 1\n    max_connections: 1\nposting:\n  groups:\n    - name: g\n").unwrap();
        // Go normalizes durations when decoding ("5m" becomes "5m0s"), so compare in that form.
        let normalized = ConfigData::from_json(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(normalized.post_check.deferred_max_backoff.0, "5m0s");
        assert_eq!(
            ConfigData::from_json(&serde_json::to_string(&normalized).unwrap()).unwrap(),
            normalized,
            "idempotent"
        );
        let dir = std::env::temp_dir().join(format!(
            "cfg-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yaml");
        save_config(&cfg, &p).unwrap();
        // a nil slice is written as `[]` and read back as an empty slice (same in Go)
        let as_json = |c: &ConfigData| {
            let mut v = serde_json::to_value(c).unwrap();
            fn walk(v: &mut Value) {
                match v {
                    Value::Object(m) => {
                        for (k, x) in m.iter_mut() {
                            if x.is_null() && SLICE_KEYS.contains(&k.as_str()) {
                                *x = Value::Array(vec![]);
                            }
                            walk(x);
                        }
                    }
                    Value::Array(a) => a.iter_mut().for_each(walk),
                    _ => {}
                }
            }
            walk(&mut v);
            v
        };
        assert_eq!(as_json(&load(&p).unwrap()), as_json(&normalized));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_config_is_migrated_and_resaved() {
        let dir = std::env::temp_dir().join(format!(
            "cfgv1-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yaml");
        std::fs::write(&p, "version: 1\nservers:\n  - host: a\n    port: 1\n    max_connections: 1\n    check_only: true\n  - host: b\n    port: 1\n    max_connections: 1\nposting:\n  groups:\n    - name: g\n").unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.version, 2);
        let roles: Vec<_> = cfg
            .servers
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| (s.role.as_str(), s.check_only))
            .collect();
        assert_eq!(roles, [("verify", None), ("upload", None)]);
        let saved = std::fs::read_to_string(&p).unwrap();
        assert!(saved.contains("version: 2") && !saved.contains("check_only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes a full default config (with a server) to `$CONFIG_OUT` so the Go loader can be run on it.
    #[test]
    #[ignore = "writes a file for the manual Go cross-check"]
    fn write_saved_yaml_for_go() {
        let mut cfg = default_config();
        cfg.servers = Some(vec![ServerConfig {
            host: "news.example.com".into(),
            port: 563,
            ssl: true,
            max_connections: 10,
            username: "u".into(),
            password: "p: #1".into(),
            proxy_url: "socks5://a:b@h:1".into(),
            ..Default::default()
        }]);
        cfg.post_check.retry_delay = Duration::new("90s");
        save_config(&cfg, Path::new(&std::env::var("CONFIG_OUT").unwrap())).unwrap();
    }

    #[test]
    fn provider_params_apply_pool_defaults() {
        let s = ServerConfig {
            host: "h".into(),
            port: 563,
            ..Default::default()
        };
        assert_eq!(
            s.provider_params(),
            ProviderParams {
                address: "h:563".into(),
                connections: 10,
                inflight: 10,
                idle_timeout_secs: 300,
                keep_alive_secs: 3600
            }
        );
    }
}
