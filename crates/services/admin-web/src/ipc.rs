//! IPC client for AdminWeb — connects to Boot over UDS.
//!
//! Mirrors the pattern used by `reloopy-admin` but adds support for the
//! EventSubscribe / CompileProgress / TestProgress message types.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UnixStream;

use reloopy_ipc::messages::{Envelope, msg_types};
use reloopy_ipc::wire;

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);
pub const IDENTITY: &str = "admin-web";
pub const EVENTS_IDENTITY: &str = "admin-web-events";

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

    /// Subscribe to events and return a receiving channel for streamed events.
    ///
    /// This spawns a dedicated reader task on the same connection. Because IPC
    /// is half-duplex after the initial handshake (request → response), event
    /// subscription must be the **last** call made on a connection — subsequent
    /// `request()` calls on the same `AdminWebIpc` will fail because the reader
    /// half is now owned by the streaming task.
    ///
    /// Callers should use a **separate** `AdminWebIpc` connection for
    /// point-in-time admin requests (status, versions, etc.) and this
    /// connection exclusively for the SSE event stream.
    pub async fn subscribe_events(
        mut self,
        event_filter: Vec<String>,
    ) -> Result<tokio::sync::mpsc::Receiver<Envelope>, Box<dyn std::error::Error + Send + Sync>> {
        // Send EventSubscribe
        let id = next_id(&self.identity);
        let envelope = Envelope {
            from: self.identity.clone(),
            to: "boot".to_string(),
            msg_type: msg_types::EVENT_SUBSCRIBE.to_string(),
            id,
            payload: serde_json::to_value(&reloopy_ipc::messages::EventSubscribe {
                event_filter: event_filter.clone(),
            })?,
            fds: Vec::new(),
        };
        wire::write_envelope(&mut self.writer, &envelope).await?;

        // Wait for EventSubscribeAck (with timeout — Boot may fail to deliver
        // the Ack if a reconnect race caused it to send to a stale routing entry)
        let ack = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            wire::read_envelope(&mut self.reader),
        )
        .await
        .map_err(|_| "Timed out waiting for EventSubscribeAck")?
        .map_err(|e| format!("Failed to read EventSubscribeAck: {}", e))?;
        if ack.msg_type != msg_types::EVENT_SUBSCRIBE_ACK {
            return Err(format!("Expected EventSubscribeAck, got {}", ack.msg_type).into());
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Envelope>(256);

        // Spawn a dedicated reader that forwards incoming event envelopes.
        tokio::spawn(async move {
            // Keep writer alive so the connection stays open (dropping it closes the socket)
            let _keep_alive_writer = self.writer;
            let mut reader = self.reader;
            loop {
                match wire::read_envelope(&mut reader).await {
                    Ok(ev) => {
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Event stream closed: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }
}
