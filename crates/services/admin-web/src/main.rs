//! Reloopy AdminWeb service.
//!
//! An independent HTTP server that provides a web dashboard for observing and
//! managing the Reloopy system. It connects to Boot's Unix Domain Socket using
//! the same IPC protocol as the admin CLI tool, then serves a REST + SSE API
//! that a browser (or any HTTP client) can consume.
//!
//! Design rationale (see plan.md §AdminWeb):
//! - Boot's TCB stays minimal — no HTTP code lands in the microkernel.
//! - AdminWeb is just another IPC peer with "admin" capability.
//! - All management is delegated to existing Admin* message types.
//! - Real-time events are forwarded over SSE using EventSubscribe.

mod ipc;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use ipc::AdminWebIpc;
use web::build_router;

pub struct Config {
    pub sock_path: PathBuf,
    pub http_addr: std::net::SocketAddr,
    pub peripheral_url: String,
}

impl Default for Config {
    fn default() -> Self {
        let base_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".reloopy");
        let http_port: u16 = std::env::var("RELOOPY_ADMIN_WEB_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7801);
        let peripheral_url = std::env::var("RELOOPY_PERIPHERAL_URL")
            .unwrap_or_else(|_| "http://localhost:7700".to_string());
        Self {
            sock_path: base_dir.join("reloopy.sock"),
            http_addr: ([127, 0, 0, 1], http_port).into(),
            peripheral_url,
        }
    }
}

pub struct AppState {
    pub ipc: Mutex<AdminWebIpc>,
    pub peripheral_url: String,
    pub sock_path: PathBuf,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::default();

    tracing::info!(
        addr = %config.http_addr,
        sock = %config.sock_path.display(),
        "AdminWeb starting"
    );

    let ipc = match AdminWebIpc::connect(&config.sock_path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                "Failed to connect to boot at {}: {}",
                config.sock_path.display(),
                e
            );
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState {
        ipc: Mutex::new(ipc),
        peripheral_url: config.peripheral_url.clone(),
        sock_path: config.sock_path.clone(),
    });

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(config.http_addr).await.unwrap_or_else(|e| {
        tracing::error!("Failed to bind HTTP listener on {}: {}", config.http_addr, e);
        std::process::exit(1);
    });

    tracing::info!(addr = %config.http_addr, "AdminWeb HTTP server listening");

    axum::serve(listener, app).await.unwrap_or_else(|e| {
        tracing::error!("HTTP server error: {}", e);
    });
}
