mod original;
mod units;

use std::net::SocketAddr;

use original::shutdown_signal;

#[tokio::main]
async fn main() {
    original::report_fallbacks();
    let original = original::Original::start().await;
    let app = units::router().with_state(original.clone());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .expect("binding the port");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serving");
    original.stop();
}
