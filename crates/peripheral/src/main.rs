mod agent;
mod deepseek;
mod ipc_client;
mod memory;
mod migration;
mod scripted_llm;
mod source;
mod tools;
mod web;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};

use reloopy_ipc::messages::{self, Envelope, msg_types};
use reloopy_ipc::LogErr;

use crate::agent::Agent;
use crate::deepseek::DeepSeekClient;
use crate::ipc_client::IpcHandle;
use crate::memory::MemoryManager;
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
    base_dir: PathBuf,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));

        let base_dir = home.join(".reloopy");

        let sock_path = std::env::var("RELOOPY_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("reloopy.sock"));

        let workspace_root = resolve_workspace_root(&base_dir)?;

        let api_key =
            std::env::var("DEEPSEEK_API_KEY").or_else(|_| read_config_api_key(&base_dir))?;

        let api_base_url = std::env::var("DEEPSEEK_BASE_URL").ok();
        let model = std::env::var("DEEPSEEK_MODEL").ok();

        let http_port = std::env::var("RELOOPY_HTTP_PORT")
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
            base_dir,
        })
    }
}

fn resolve_workspace_root(base_dir: &PathBuf) -> Result<PathBuf, String> {
    // 1. Explicit override via environment variable.
    if let Ok(ws) = std::env::var("RELOOPY_WORKSPACE") {
        let path = PathBuf::from(ws);
        if path.join("crates").join("peripheral").exists() {
            return Ok(path);
        }
        return Err(format!(
            "RELOOPY_WORKSPACE={} does not contain crates/peripheral/",
            path.display()
        ));
    }

    // 2. Default managed workspace: ~/.reloopy/workspace
    let default_ws = base_dir.join("workspace");
    if default_ws.join("crates").join("peripheral").exists() {
        return Ok(default_ws);
    }

    // 3. Evolved source after a hot-swap: ~/.reloopy/peripheral/current/source/
    let evolved_source = base_dir.join("peripheral").join("current").join("source");
    if evolved_source.join("crates").join("peripheral").exists() {
        return Ok(evolved_source);
    }

    Err(
        "Cannot determine workspace root. Set RELOOPY_WORKSPACE env var, \
         populate ~/.reloopy/workspace, or ensure ~/.reloopy/peripheral/current/source/ exists."
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
    Err("DEEPSEEK_API_KEY not set and not found in ~/.reloopy/config.json".to_string())
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
        "reloopy-peripheral starting"
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
    let memory = MemoryManager::new(&config.base_dir);

    run(
        deepseek,
        source,
        memory,
        ipc,
        config.heartbeat_interval,
        config.http_port,
    )
    .await;
}

async fn run(
    llm: DeepSeekClient,
    source: SourceManager,
    memory: MemoryManager,
    ipc: IpcHandle,
    heartbeat_interval: Duration,
    http_port: u16,
) {
    let ipc_tx = ipc.tx;
    let runlevel = ipc.runlevel;
    let inherited_listener = ipc.inherited_listener;

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

    // Create the HTTP listener early so the IPC handler can reference it for
    // the hot-swap handoff (PrepareHandoff → HandoffReady FD passing).
    let std_listener = if let Some(fd) = inherited_listener {
        #[cfg(unix)]
        {
            use std::os::unix::io::FromRawFd;
            use std::os::unix::io::IntoRawFd;

            let raw = fd.into_raw_fd();
            let std_listener = unsafe { std::net::TcpListener::from_raw_fd(raw) };
            tracing::info!("Using inherited HTTP listener fd");
            std_listener
        }
        #[cfg(not(unix))]
        {
            tracing::error!("Inherited listener not supported on non-Unix platforms");
            std::process::exit(1);
        }
    } else {
        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], http_port));
        match std::net::TcpListener::bind(addr) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    "Failed to bind HTTP on {}: {}; falling back to OS-assigned port",
                    addr,
                    e
                );
                let fallback = std::net::SocketAddr::from(([0, 0, 0, 0], 0u16));
                match std::net::TcpListener::bind(fallback) {
                    Ok(l) => l,
                    Err(e2) => {
                        tracing::error!("Failed to bind HTTP on fallback port: {}", e2);
                        std::process::exit(1);
                    }
                }
            }
        }
    };

    std_listener
        .set_nonblocking(true)
        .unwrap_or_else(|e| tracing::warn!("Failed to set listener nonblocking: {}", e));

    let std_listener = Arc::new(std_listener);

    let shutdown_for_ipc = shutdown_notify.clone();
    let message_tx = update_result_tx.clone();
    let mut ipc_rx = ipc.rx;
    let ipc_tx_for_ipc = ipc_tx.clone();
    let listener_for_ipc = std_listener.clone();
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
                    message_tx.send(envelope).await.warn_err();
                    shutdown_for_ipc.notify_waiters();
                    break;
                }
                msg_types::RUNLEVEL_CHANGE => {
                    tracing::info!("Runlevel change: {}", envelope.payload);
                }
                msg_types::PREPARE_HANDOFF => {
                    tracing::info!("PrepareHandoff received — sending listener fd");
                    #[cfg(unix)]
                    {
                        use std::os::unix::io::{FromRawFd, IntoRawFd, OwnedFd};

                        let duplicate: std::net::TcpListener =
                            match listener_for_ipc.try_clone() {
                                Ok(l) => l,
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to clone listener for handoff: {}",
                                        e
                                    );
                                    continue;
                                }
                            };
                        let fd = duplicate.into_raw_fd();
                        unsafe {
                            let flags = libc::fcntl(fd, libc::F_GETFD);
                            if flags >= 0 {
                                let _ = libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                            }
                        }
                        let handoff = Envelope {
                            from: "peripheral".to_string(),
                            to: "boot".to_string(),
                            msg_type: msg_types::HANDOFF_READY.to_string(),
                            id: ipc_client::new_msg_id(),
                            payload: serde_json::to_value(&messages::HandoffReady)
                                .unwrap_or_default(),
                            fds: vec![Arc::new(unsafe { OwnedFd::from_raw_fd(fd) })],
                        };
                        ipc_tx_for_ipc.send(handoff).await.warn_err();
                    }
                }
                msg_types::UPDATE_ACCEPTED | msg_types::UPDATE_REJECTED => {
                    message_tx.send(envelope).await.warn_err();
                }
                other => {
                    tracing::debug!(msg_type = %other, "Unhandled message");
                }
            }
        }
    });

    let agent = Agent::new(llm, source, memory, ipc_tx, update_result_rx);

    let app_state = Arc::new(AppState {
        agent: Mutex::new(agent),
    });

    let router = web::build_router(app_state);

    let listener = match TcpListener::from_std((*std_listener).try_clone().unwrap()) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to adopt listener: {}", e);
            std::process::exit(1);
        }
    };

    let actual_addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to retrieve local address from listener: {}", e);
            std::process::exit(1);
        }
    };
    tracing::info!("HTTP server listening on http://{}", actual_addr);

    let shutdown = shutdown_notify.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await
        .unwrap_or_else(|e| tracing::error!("HTTP server error: {}", e));
}
