//! IPC client for AdminWeb — connects to Boot over UDS.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UnixStream;

use reloopy_ipc::messages::{Envelope, HealthReport, LeaseRenew, msg_types};
use reloopy_ipc::wire;

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);
pub const IDENTITY: &str = "admin-web";

fn next_id(identity: &str) -> String {
    format!("{}-{}", identity, MSG_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub struct AdminWebIpc {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    identity: String,
}

impl AdminWebIpc {
    pub async fn connect(socket_path: &PathBuf, identity: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read_half, write_half) = stream.into_split();
        let mut client = Self {
            reader: tokio::io::BufReader::new(read_half),
            writer: write_half,
            identity: identity.to_string(),
        };
        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let hello = Envelope {
            from: self.identity.clone(),
            to: "boot".to_string(),
            msg_type: msg_types::HELLO.to_string(),
            id: next_id(&self.identity),
            payload: serde_json::to_value(&reloopy_ipc::messages::Hello {
                protocol_version: "1.0".to_string(),
                capabilities: serde_json::json!(["admin"]),
            })?,
            fds: Vec::new(),
        };

        wire::write_envelope(&mut self.writer, &hello).await?;
        let welcome = wire::read_envelope(&mut self.reader).await?;

        if welcome.msg_type != msg_types::WELCOME {
            return Err(format!("Expected Welcome, got {}", welcome.msg_type).into());
        }

        Ok(())
    }

    /// Send a request and wait for the matching response envelope.
    pub async fn request(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Result<Envelope, Box<dyn std::error::Error + Send + Sync>> {
        let id = next_id(&self.identity);
        let envelope = Envelope {
            from: self.identity.clone(),
            to: "boot".to_string(),
            msg_type: msg_type.to_string(),
            id: id.clone(),
            payload,
            fds: Vec::new(),
        };

        wire::write_envelope(&mut self.writer, &envelope).await?;

        let timeout = tokio::time::Duration::from_secs(10);
        match tokio::time::timeout(timeout, wire::read_envelope(&mut self.reader)).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("Request timed out".into()),
        }
    }

    /// Send a LeaseRenew heartbeat to keep the Boot lease alive.
    pub async fn heartbeat(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let health = HealthReport {
            runlevel: 2,
            memory_bytes: 0,
            cpu_percent: 0.0,
            tasks_processed: 0,
        };
        let payload = serde_json::to_value(&LeaseRenew { health })?;
        self.request(msg_types::LEASE_RENEW, payload).await?;
        Ok(())
    }
}
