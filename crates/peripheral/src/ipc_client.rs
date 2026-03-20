use std::os::unix::io::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UnixStream;
use tokio::sync::mpsc;

use reloopy_ipc::messages::{Envelope, HealthReport, Hello, LeaseRenew, Welcome, msg_types};
use reloopy_ipc::wire;

const IDENTITY: &str = "peripheral";

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn new_msg_id() -> String {
    format!(
        "{}-{}",
        IDENTITY,
        MSG_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub struct IpcHandle {
    pub tx: mpsc::Sender<Envelope>,
    pub rx: mpsc::Receiver<Envelope>,
    pub runlevel: u8,
    pub inherited_listener: Option<OwnedFd>,
}

pub async fn connect_and_handshake(
    sock_path: &Path,
) -> Result<IpcHandle, Box<dyn std::error::Error + Send + Sync>> {
    let stream = UnixStream::connect(sock_path).await?;

    let hello = Hello {
        protocol_version: "1.0".to_string(),
        capabilities: serde_json::json!(["agent"]),
    };

    let hello_envelope = Envelope {
        from: IDENTITY.to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::HELLO.to_string(),
        id: new_msg_id(),
        payload: serde_json::to_value(&hello)?,
        fds: Vec::new(),
    };

    wire::write_envelope_with_fds(&stream, &hello_envelope).await?;
    tracing::info!("Hello sent, waiting for Welcome...");

    let welcome_envelope = wire::read_envelope_with_fds(&stream).await?;
    if welcome_envelope.msg_type != msg_types::WELCOME {
        return Err(format!("Expected Welcome, got: {}", welcome_envelope.msg_type).into());
    }

    let welcome: Welcome = serde_json::from_value(welcome_envelope.payload)?;
    let inherited_listener = welcome_envelope
        .fds
        .first()
        .cloned()
        .and_then(|fd| Arc::try_unwrap(fd).ok());
    tracing::info!(runlevel = welcome.runlevel, "Handshake complete");

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Envelope>(64);
    let (incoming_tx, incoming_rx) = mpsc::channel::<Envelope>(64);

    let write_stream = stream.try_clone()?;
    tokio::spawn(async move {
        while let Some(envelope) = outgoing_rx.recv().await {
            if let Err(e) = wire::write_envelope_with_fds(&write_stream, &envelope).await {
                tracing::error!("IPC write error: {}", e);
                break;
            }
        }
    });

    let read_stream = stream;
    tokio::spawn(async move {
        loop {
            match wire::read_envelope_with_fds(&read_stream).await {
                Ok(envelope) => {
                    if incoming_tx.send(envelope).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::info!("IPC read ended: {}", e);
                    break;
                }
            }
        }
    });

    Ok(IpcHandle {
        tx: outgoing_tx,
        rx: incoming_rx,
        runlevel: welcome.runlevel,
        inherited_listener,
    })
}

pub fn make_heartbeat(runlevel: u8) -> Envelope {
    let health = HealthReport {
        runlevel,
        memory_bytes: 0,
        cpu_percent: 0.0,
        tasks_processed: 0,
    };

    let renew = LeaseRenew { health };
    Envelope {
        from: IDENTITY.to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::LEASE_RENEW.to_string(),
        id: new_msg_id(),
        payload: serde_json::to_value(&renew).unwrap_or_default(),
        fds: Vec::new(),
    }
}

pub fn make_submit_update(source_path: &str) -> Envelope {
    let submit = reloopy_ipc::messages::SubmitUpdate {
        source_path: source_path.to_string(),
    };
    Envelope {
        from: IDENTITY.to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::SUBMIT_UPDATE.to_string(),
        id: new_msg_id(),
        payload: serde_json::to_value(&submit).unwrap_or_default(),
        fds: Vec::new(),
    }
}
