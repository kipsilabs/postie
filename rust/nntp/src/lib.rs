//! NNTP client for posting articles and checking their presence.
//!
//! * [`conn`]: one connection as a single-task state machine. Up to `inflight` requests are on
//!   the wire at once. `POST` is two-phase as RFC 3977 requires (send `POST`, wait for `340`, then
//!   send the body), and `STAT` is pipelined; replies are matched in order.
//! * [`pool`]: a lazy per-provider connection pool fed by one shared queue (idle connections pull
//!   work, so load balances by itself) and a [`pool::Client`] that spreads work round-robin across
//!   providers.
//! * [`throttle`]: a shared byte-rate limiter applied before each body is sent.
//!
//! Traps from the research notes (`rust/research/nntp.md`): only an explicit `240` is a successful
//! post (a `441` can mean the article already exists, so it is reported as a rejection and the
//! caller decides); `STAT` answering `430` means not found, anything unexpected is an error, never
//! "missing"; when a connection drops every unanswered request is failed with
//! [`NntpError::Disconnected`] so callers requeue it instead of treating it as missing.

pub mod conn;
pub mod pool;
pub mod throttle;
mod tls;

use std::time::Duration;

/// Errors a request can end with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NntpError {
    #[error("connect: {0}")]
    Connect(String),
    #[error("authentication failed: {code} {text}")]
    Auth { code: u16, text: String },
    #[error("unexpected response: {code} {text}")]
    Protocol { code: u16, text: String },
    /// The server refused the article (e.g. `441`); `code` is the NNTP status.
    #[error("post rejected: {code} {text}")]
    PostRejected { code: u16, text: String },
    /// The connection ended while the request was in flight (safe to retry).
    #[error("connection lost")]
    Disconnected,
    #[error("timeout")]
    Timeout,
    #[error("client closed")]
    Closed,
}

impl NntpError {
    /// True for failures where retrying the same request elsewhere is reasonable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Disconnected | Self::Timeout)
    }
}

/// Connects, reads the greeting and authenticates once, then hangs up; the elapsed time is the
/// round trip a settings page shows. Used to validate a server before saving it.
pub async fn test_provider(endpoint: &Endpoint) -> Result<Duration, NntpError> {
    let start = std::time::Instant::now();
    conn::open(endpoint).await.map(|_| start.elapsed())
}

/// SOCKS5 proxy (`socks5://user:pass@host:port` or plain `host:port`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5 {
    pub addr: String,
    pub auth: Option<(String, String)>,
}

impl Socks5 {
    /// Parses both the documented URL form and a bare `host:port`.
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s
            .strip_prefix("socks5://")
            .or_else(|| s.strip_prefix("socks5h://"))
            .unwrap_or(s);
        if rest.is_empty() {
            return None;
        }
        match rest.rsplit_once('@') {
            Some((cred, addr)) => {
                let (u, p) = cred.split_once(':').unwrap_or((cred, ""));
                Some(Self {
                    addr: addr.to_string(),
                    auth: Some((u.to_string(), p.to_string())),
                })
            }
            None => Some(Self {
                addr: rest.to_string(),
                auth: None,
            }),
        }
    }
}

/// Everything needed to open connections to one provider.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// Skip certificate verification (`insecure_ssl`).
    pub insecure_tls: bool,
    pub username: String,
    pub password: String,
    pub proxy: Option<Socks5>,
    pub connect_timeout: Duration,
}

impl Endpoint {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            tls: false,
            insecure_tls: false,
            username: String::new(),
            password: String::new(),
            proxy: None,
            connect_timeout: Duration::from_secs(30),
        }
    }

    /// `host:port`, the provider's display key (`host:port+user` when a username is set).
    pub fn key(&self) -> String {
        if self.username.is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            format!("{}:{}+{}", self.host, self.port, self.username)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks5_url_and_bare_forms_parse() {
        assert_eq!(
            Socks5::parse("socks5://u:p@px:1080"),
            Some(Socks5 {
                addr: "px:1080".into(),
                auth: Some(("u".into(), "p".into()))
            })
        );
        assert_eq!(
            Socks5::parse("px:1080"),
            Some(Socks5 {
                addr: "px:1080".into(),
                auth: None
            })
        );
        assert_eq!(
            Socks5::parse("socks5://px:1"),
            Some(Socks5 {
                addr: "px:1".into(),
                auth: None
            })
        );
        assert_eq!(
            Socks5::parse("socks5://u@px:1").unwrap().auth,
            Some(("u".into(), String::new()))
        );
        assert_eq!(Socks5::parse(""), None);
    }

    #[test]
    fn provider_key_matches_nntppool_naming() {
        let mut e = Endpoint::new("h", 563);
        assert_eq!(e.key(), "h:563");
        e.username = "u".into();
        assert_eq!(e.key(), "h:563+u");
    }

    #[test]
    fn retryability() {
        assert!(NntpError::Disconnected.is_retryable() && NntpError::Timeout.is_retryable());
        assert!(
            !NntpError::PostRejected {
                code: 441,
                text: String::new()
            }
            .is_retryable()
        );
    }
}
