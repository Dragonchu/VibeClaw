//! Reloopy compiler service.
//!
//! Connects to Boot's Unix Domain Socket, completes handshake,
//! and maintains a heartbeat lease. In later phases, handles
//! compilation requests from the update pipeline.

use std::path::PathBuf;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::process::Command;

use reloopy_ipc::messages::{
    CompileRequest, CompileResult, Envelope, HealthReport, Hello, LeaseRenew, Welcome, msg_types,
};
use reloopy_ipc::wire;
use tracing::{error, info, warn};

const IDENTITY: &str = "compiler";

#[derive(Debug, Clone)]
struct Config {
    sock_path: PathBuf,
    heartbeat_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        let base_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".reloopy");
        Self {
            sock_path: base_dir.join("reloopy.sock"),
            heartbeat_interval: Duration::from_secs(8),
        }
    }
}

fn new_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("{}-{}", IDENTITY, COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("reloopy-compiler service starting");

    let config = Config::default();

    loop {
        match run_service(&config).await {
            Ok(()) => {
                info!("Service exited cleanly");
                break;
            }
            Err(e) => {
                error!("Service error: {}. Reconnecting in 5s...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn run_service(config: &Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!(sock = %config.sock_path.display(), "Connecting to Boot");
    let stream = UnixStream::connect(&config.sock_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    let hello = Hello {
        protocol_version: "1.0".to_string(),
        capabilities: serde_json::json!(["compile"]),
    };

    let hello_envelope = Envelope {
        from: IDENTITY.to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::HELLO.to_string(),
        id: new_msg_id(),
        payload: serde_json::to_value(&hello)?,
        fds: Vec::new(),
    };

    wire::write_envelope(&mut writer, &hello_envelope).await?;
    info!("Hello sent, waiting for Welcome...");

    let welcome_envelope = wire::read_envelope(&mut reader).await?;

    if welcome_envelope.msg_type != msg_types::WELCOME {
        return Err(format!(
            "Expected Welcome, got: {} (payload: {})",
            welcome_envelope.msg_type, welcome_envelope.payload
        )
        .into());
    }

    let welcome: Welcome = serde_json::from_value(welcome_envelope.payload)?;
    info!(
        runlevel = welcome.runlevel,
        "Handshake complete — connected to Boot"
    );

    let mut heartbeat_interval = tokio::time::interval(config.heartbeat_interval);
    let mut tasks_processed: u64 = 0;

    loop {
        tokio::select! {
            _ = heartbeat_interval.tick() => {
                let health = HealthReport {
                    runlevel: welcome.runlevel,
                    memory_bytes: 0,
                    cpu_percent: 0.0,
                    tasks_processed,
                };

                let renew = LeaseRenew { health };
                let envelope = Envelope {
                    from: IDENTITY.to_string(),
                    to: "boot".to_string(),
                    msg_type: msg_types::LEASE_RENEW.to_string(),
                    id: new_msg_id(),
                    payload: serde_json::to_value(&renew)?,
                fds: Vec::new(),
                };

                wire::write_envelope(&mut writer, &envelope).await?;
                tracing::trace!("Heartbeat sent");
            }

            result = wire::read_envelope(&mut reader) => {
                let envelope = result?;

                match envelope.msg_type.as_str() {
                    msg_types::LEASE_ACK => {
                        tracing::trace!("LeaseAck received");
                    }
                    msg_types::SHUTDOWN => {
                        info!("Shutdown received: {}", envelope.payload);
                        return Ok(());
                    }
                    msg_types::RUNLEVEL_CHANGE => {
                        info!("Runlevel change: {}", envelope.payload);
                    }
                    msg_types::COMPILE_REQUEST => {
                        let result = handle_compile_request(&envelope).await;
                        let response = Envelope {
                            from: IDENTITY.to_string(),
                            to: envelope.from.clone(),
                            msg_type: msg_types::COMPILE_RESULT.to_string(),
                            id: envelope.id.clone(),
                            payload: serde_json::to_value(&result)?,
                        fds: Vec::new(),
                        };
                        wire::write_envelope(&mut writer, &response).await?;
                        tasks_processed += 1;
                    }
                    other => {
                        warn!("Unhandled message type: {}", other);
                    }
                }
            }
        }
    }
}

async fn handle_compile_request(envelope: &Envelope) -> CompileResult {
    let request: CompileRequest = match serde_json::from_value(envelope.payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            return CompileResult {
                version: String::new(),
                success: false,
                binary_path: None,
                errors: Some(format!("Invalid CompileRequest payload: {}", e)),
                fds: Vec::new(),
            };
        }
    };

    info!(
        version = %request.version,
        source = %request.source_path,
        output = %request.output_path,
        "Compiling"
    );

    let source_path = PathBuf::from(&request.source_path);
    if !source_path.exists() {
        return CompileResult {
            version: request.version,
            success: false,
            binary_path: None,
            errors: Some(format!(
                "Source path does not exist: {}",
                request.source_path
            )),
        };
    }

    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("-p")
        .arg("reloopy-peripheral")
        .arg("--target-dir")
        .arg(&request.output_path)
        .current_dir(&request.source_path)
        .output()
        .await;

    match output {
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr).to_string();
            if result.status.success() {
                let binary_path = PathBuf::from(&request.output_path)
                    .join("release")
                    .join("reloopy-peripheral");
                let binary_str = binary_path.to_string_lossy().to_string();

                info!(version = %request.version, binary = %binary_str, "Compilation succeeded");
                CompileResult {
                    version: request.version,
                    success: true,
                    binary_path: Some(binary_str),
                    errors: None,
                }
            } else {
                warn!(version = %request.version, "Compilation failed");
                CompileResult {
                    version: request.version,
                    success: false,
                    binary_path: None,
                    errors: Some(stderr),
                }
            }
        }
        Err(e) => {
            error!(version = %request.version, "Failed to invoke cargo: {}", e);
            CompileResult {
                version: request.version,
                success: false,
                binary_path: None,
                errors: Some(format!("Failed to invoke cargo: {}", e)),
            }
        }
    }
}
