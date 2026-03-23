use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UnixStream;

use reloopy_ipc::messages::{Envelope, msg_types};
use reloopy_ipc::wire;

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("admin-{}", MSG_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub struct AdminClient {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl AdminClient {
    pub async fn connect(
        socket_path: &PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read_half, write_half) = stream.into_split();
        let mut client = Self {
            reader: tokio::io::BufReader::new(read_half),
            writer: write_half,
        };
        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let hello = Envelope {
            from: "admin".to_string(),
            to: "boot".to_string(),
            msg_type: msg_types::HELLO.to_string(),
            id: next_id(),
            payload: serde_json::to_value(&reloopy_ipc::messages::Hello {
                protocol_version: "1.0".to_string(),
                capabilities: serde_json::json!(["admin"]),
                http_port: None,
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

    pub async fn request(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Result<Envelope, Box<dyn std::error::Error + Send + Sync>> {
        let id = next_id();
        let envelope = Envelope {
            from: "admin".to_string(),
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
}
