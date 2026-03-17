mod agent;
mod deepseek;
mod ipc_client;
mod migration;
mod source;
mod tools;
mod web;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc};

use loopy_ipc::messages::{Envelope, msg_types};

use crate::agent::Agent;
use crate::deepseek::DeepSeekClient;
use crate::ipc_client::IpcHandle;
use crate::source::SourceManager;
use crate::web::AppState;

const DEFAULT_HTTP_PORT: u16 = 7700;

struct Config {
    sock_path: PathBuf,
    workspace_root: PathBuf,
    heartbeat_interval: Duration,
    api_key: String,
    api_base_url: Option<String>,
    model: Option<String>,
    http_port: u16,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));

        let base_dir = home.join(".loopy");

        let sock_path = std::env::var("LOOPY_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("loopy.sock"));

        let workspace_root = resolve_workspace_root(&base_dir)?;

        let api_key =
            std::env::var("DEEPSEEK_API_KEY").or_else(|_| read_config_api_key(&base_dir))?;

        let api_base_url = std::env::var("DEEPSEEK_BASE_URL").ok();
        let model = std::env::var("DEEPSEEK_MODEL").ok();

        let http_port = std::env::var("LOOPY_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_HTTP_PORT);

        Ok(Self {
            sock_path,
            workspace_root,
            heartbeat_interval: Duration::from_secs(8),
            api_key,
            api_base_url,
            model,
            http_port,
        })
    }
}

fn resolve_workspace_root(base_dir: &PathBuf) -> Result<PathBuf, String> {
    if let Ok(ws) = std::env::var("LOOPY_WORKSPACE") {
        let path = PathBuf::from(ws);
        if path.join("crates").join("peripheral").exists() {
            return Ok(path);
        }
        return Err(format!(
            "LOOPY_WORKSPACE={} does not contain crates/peripheral/",
            path.display()
        ));
    }

    let evolved_source = base_dir.join("peripheral").join("current").join("source");
    if evolved_source.join("crates").join("peripheral").exists() {
        return Ok(evolved_source);
    }

    Err(
        "Cannot determine workspace root. Set LOOPY_WORKSPACE env var or ensure ~/.loopy/peripheral/current/source/ exists."
            .to_string(),
    )
}

fn read_config_api_key(base_dir: &PathBuf) -> Result<String, String> {
    let config_path = base_dir.join("config.json");
    if config_path.exists() {
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| format!("Read config: {}", e))?;
        let config: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Parse config: {}", e))?;
        if let Some(key) = config["deepseek_api_key"].as_str() {
            return Ok(key.to_string());
        }
    }
    Err("DEEPSEEK_API_KEY not set and not found in ~/.loopy/config.json".to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Configuration error: {}", e);
            std::process::exit(1);
        }
    };

    tracing::info!(
        workspace = %config.workspace_root.display(),
        sock = %config.sock_path.display(),
        http_port = config.http_port,
        "loopy-peripheral starting"
    );

    let ipc = match ipc_client::connect_and_handshake(&config.sock_path).await {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!("Failed to connect to Boot: {}", e);
            std::process::exit(1);
        }
    };

    let deepseek = DeepSeekClient::new(config.api_key, config.api_base_url, config.model);
    let source = SourceManager::new(config.workspace_root);
    let agent = Agent::new(deepseek, source);

    run(agent, ipc, config.heartbeat_interval, config.http_port).await;
}

async fn run(agent: Agent, ipc: IpcHandle, heartbeat_interval: Duration, http_port: u16) {
    let ipc_tx = ipc.tx;
    let runlevel = ipc.runlevel;

    let heartbeat_tx = ipc_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(heartbeat_interval);
        loop {
            interval.tick().await;
            let hb = ipc_client::make_heartbeat(runlevel);
            if heartbeat_tx.send(hb).await.is_err() {
                break;
            }
        }
    });

    let (update_result_tx, update_result_rx) = mpsc::channel::<Envelope>(4);
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());

    let shutdown_for_ipc = shutdown_notify.clone();
    let message_tx = update_result_tx.clone();
    let mut ipc_rx = ipc.rx;
    tokio::spawn(async move {
        while let Some(envelope) = ipc_rx.recv().await {
            match envelope.msg_type.as_str() {
                msg_types::LEASE_ACK => {
                    tracing::trace!("LeaseAck received");
                }
                msg_types::SHUTDOWN => {
                    let reason = envelope
                        .payload
                        .get("reason")
                        .and_then(|v: &serde_json::Value| v.as_str())
                        .unwrap_or("unknown");
                    tracing::info!(%reason, "Shutdown received");
                    let _ = message_tx.send(envelope).await;
                    shutdown_for_ipc.notify_waiters();
                    break;
                }
                msg_types::RUNLEVEL_CHANGE => {
                    tracing::info!("Runlevel change: {}", envelope.payload);
                }
                msg_types::UPDATE_ACCEPTED | msg_types::UPDATE_REJECTED => {
                    let _ = message_tx.send(envelope).await;
                }
                other => {
                    tracing::debug!(msg_type = %other, "Unhandled message");
                }
            }
        }
    });

    let app_state = Arc::new(AppState {
        agent: Mutex::new(agent),
        ipc_tx,
        update_result_rx: Mutex::new(update_result_rx),
    });

    let router = web::build_router(app_state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], http_port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind HTTP on {}: {}", addr, e);
            std::process::exit(1);
        }
    };

    tracing::info!("HTTP server listening on http://{}", addr);

    let shutdown = shutdown_notify.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await
        .unwrap_or_else(|e| tracing::error!("HTTP server error: {}", e));
}
