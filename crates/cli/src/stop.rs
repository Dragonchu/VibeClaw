//! Lightweight shutdown client — connects to Boot's UDS and sends an
//! AdminShutdownRequest, then waits for acknowledgement.

use std::path::PathBuf;

use tokio::net::UnixStream;

use reloopy_ipc::messages::{self, Envelope, msg_types};
use reloopy_ipc::wire;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub async fn send_shutdown(socket_path: &PathBuf, reason: &str) -> Result<(), BoxError> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        format!(
            "Cannot connect to boot at {}: {} — is reloopy running?",
            socket_path.display(),
            e
        )
    })?;

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);

    // Handshake
    let hello = Envelope {
        from: "cli".to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::HELLO.to_string(),
        id: "cli-1".to_string(),
        payload: serde_json::to_value(&messages::Hello {
            protocol_version: "1.0".to_string(),
            capabilities: serde_json::json!(["admin"]),
            http_port: None,
        })?,
        fds: Vec::new(),
    };
    wire::write_envelope(&mut write_half, &hello).await?;

    let welcome = wire::read_envelope(&mut reader).await?;
    if welcome.msg_type != msg_types::WELCOME {
        return Err(format!("Expected Welcome, got {}", welcome.msg_type).into());
    }

    // Send shutdown request
    let shutdown = Envelope {
        from: "cli".to_string(),
        to: "boot".to_string(),
        msg_type: msg_types::ADMIN_SHUTDOWN_REQUEST.to_string(),
        id: "cli-2".to_string(),
        payload: serde_json::to_value(&messages::AdminShutdownRequest {
            reason: reason.to_string(),
        })?,
        fds: Vec::new(),
    };
    wire::write_envelope(&mut write_half, &shutdown).await?;

    // Wait for response (with timeout)
    let timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(timeout, wire::read_envelope(&mut reader)).await {
        Ok(Ok(resp)) => {
            let data: messages::AdminShutdownResponse = serde_json::from_value(resp.payload)?;
            if data.success {
                Ok(())
            } else {
                Err(format!(
                    "Shutdown rejected: {}",
                    data.error.as_deref().unwrap_or("unknown")
                )
                .into())
            }
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("Shutdown request timed out".into()),
    }
}
