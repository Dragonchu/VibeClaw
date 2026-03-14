use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use tokio::net::UnixStream;

use loopy_ipc::messages::{
    AuditLog, Envelope, HealthReport, Hello, LeaseRenew, Welcome, msg_types,
};
use loopy_ipc::wire;
use tracing::{error, info, warn};

const IDENTITY: &str = "audit";

#[derive(Debug, Clone)]
struct Config {
    sock_path: PathBuf,
    audit_dir: PathBuf,
    heartbeat_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        let base_dir = home.join(".loopy");
        Self {
            sock_path: base_dir.join("loopy.sock"),
            audit_dir: base_dir.join("audit"),
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

    info!("loopy-audit service starting");

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
    fs::create_dir_all(&config.audit_dir)?;

    info!(sock = %config.sock_path.display(), "Connecting to Boot");
    let stream = UnixStream::connect(&config.sock_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    let hello = Hello {
        protocol_version: "1.0".to_string(),
        capabilities: serde_json::json!(["log_write", "log_query"]),
    };

    let hello_envelope = Envelope {
        from: IDENTITY.to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::HELLO.to_string(),
        id: new_msg_id(),
        payload: serde_json::to_value(&hello)?,
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
                    msg_types::AUDIT_LOG => {
                        handle_audit_log(&envelope, &config.audit_dir);
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

fn handle_audit_log(envelope: &Envelope, audit_dir: &PathBuf) {
    let entry: AuditLog = match serde_json::from_value(envelope.payload.clone()) {
        Ok(e) => e,
        Err(e) => {
            warn!("Invalid AuditLog payload: {}", e);
            return;
        }
    };

    let log_path = audit_dir.join("judgments.log");

    let line = match serde_json::to_string(&entry) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to serialize audit entry: {}", e);
            return;
        }
    };

    match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", line) {
                error!("Failed to write audit log: {}", e);
            } else {
                tracing::debug!(
                    event = %entry.event,
                    version = ?entry.version,
                    "Audit entry written"
                );
            }
        }
        Err(e) => {
            error!("Failed to open audit log {}: {}", log_path.display(), e);
        }
    }
}
