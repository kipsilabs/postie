//! Port of `cmd/web`: the HTTP/WebSocket API and static frontend over `postie-core`.

pub mod auth;
pub mod routes;
pub mod static_files;
pub mod util;

use std::path::PathBuf;
use std::sync::Arc;

use postie_core::backend::app::App;
use postie_core::webauth;
use postie_core::wshub::Hub;

/// Shared server state.
pub struct Srv {
    pub app: Arc<App>,
    pub auth: Option<Arc<webauth::Service>>,
    pub hub: Hub,
    pub auth_required: bool,
    pub limiter: auth::Limiter,
    pub build_dir: PathBuf,
}

impl Srv {
    pub fn new(
        app: Arc<App>,
        auth: Option<Arc<webauth::Service>>,
        auth_required: bool,
        build_dir: PathBuf,
    ) -> Arc<Self> {
        let hub = Hub::new();
        let h = hub.clone();
        app.set_event_emitter(Arc::new(move |kind, data| match data {
            Some(d) => h.emit_event(kind, d),
            None => h.emit_event(kind, serde_json::Value::Null),
        }));
        Arc::new(Self {
            app,
            auth,
            hub,
            auth_required,
            limiter: auth::Limiter::default(),
            build_dir,
        })
    }
}

/// A server that is accepting connections.
pub struct Running {
    pub addr: std::net::SocketAddr,
    pub shutdown: tokio_util::sync::CancellationToken,
    pub task: tokio::task::JoinHandle<()>,
}

/// Binds `addr` (`host:port`; port 0 picks a free one) and serves the API until `shutdown` fires.
pub async fn serve(srv: Arc<Srv>, addr: &str) -> std::io::Result<Running> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let shutdown = tokio_util::sync::CancellationToken::new();
    let token = shutdown.clone();
    let router = routes::router(srv).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await;
    });
    Ok(Running {
        addr: local,
        shutdown,
        task,
    })
}
