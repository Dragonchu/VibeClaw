//! Reloopy AdminWeb service.
//!
//! An independent HTTP server that provides a web dashboard for observing and
//! managing the Reloopy system. It connects to Boot's Unix Domain Socket using
//! the same IPC protocol as the admin CLI tool, then serves a REST API that a
//! browser (or any HTTP client) can consume. The frontend polls for updates.
//!
//! Design rationale (see plan.md §AdminWeb):
//! - Boot's TCB stays minimal — no HTTP code lands in the microkernel.
//! - AdminWeb is just another IPC peer with "admin" capability.
//! - All management is delegated to existing Admin* message types.

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
    pub workspace_root: PathBuf,
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
        let workspace_root = std::env::var("RELOOPY_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("workspace"));
        Self {
            sock_path: base_dir.join("reloopy.sock"),
            http_addr: ([127, 0, 0, 1], http_port).into(),
            peripheral_url,
            workspace_root,
        }
    }
}

pub struct AppState {
    pub ipc: Mutex<AdminWebIpc>,
    pub peripheral_url: String,
    pub workspace_root: PathBuf,
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

    let ipc = match AdminWebIpc::connect(&config.sock_path, ipc::IDENTITY).await {
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
        workspace_root: config.workspace_root.clone(),
    });

    // Keep Boot lease alive — admin-web must heartbeat just like any other peer.
    let heartbeat_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(8));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            let mut ipc = heartbeat_state.ipc.lock().await;
            if let Err(e) = ipc.heartbeat().await {
                tracing::warn!("Heartbeat failed, admin-web will lose its Boot lease: {}", e);
                break;
            }
            tracing::trace!("Heartbeat sent");
        }
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
