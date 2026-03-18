//! IPC router over Unix Domain Sockets.
//!
//! The router listens on a UDS, accepts connections from services and peripherals,
//! and routes messages based on the `to` field in the envelope.
//!
//! Wire format: `[4-byte big-endian length][JSON bytes]`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, mpsc};

use reloopy_ipc::messages::Envelope;
use reloopy_ipc::wire;

/// A handle representing a connected peer.
#[derive(Debug)]
pub struct PeerHandle {
    /// Peer identity (e.g. "peripheral", "compiler", "judge", "audit")
    pub identity: String,
    /// Channel to send messages to this peer
    pub tx: mpsc::Sender<Envelope>,
}

/// The IPC router. Owns the Unix socket listener and maintains
/// a table of connected peers.
pub struct IpcRouter {
    /// Path to the Unix Domain Socket
    sock_path: PathBuf,
    /// Connected peers keyed by identity
    peers: Arc<RwLock<HashMap<String, PeerHandle>>>,
    /// Channel for delivering messages addressed to "boot"
    boot_rx: mpsc::Receiver<Envelope>,
    /// Sender side kept for cloning into connection handlers
    boot_tx: mpsc::Sender<Envelope>,
}

impl IpcRouter {
    /// Create a new IPC router bound to the given socket path.
    pub fn new(sock_path: PathBuf) -> Self {
        let (boot_tx, boot_rx) = mpsc::channel(256);
        Self {
            sock_path,
            peers: Arc::new(RwLock::new(HashMap::new())),
            boot_rx,
            boot_tx,
        }
    }

    /// Get the path to the socket file.
    pub fn sock_path(&self) -> &Path {
        &self.sock_path
    }

    /// Take the receiver for messages addressed to "boot".
    /// This should be called once by the microkernel.
    pub fn take_boot_rx(&mut self) -> mpsc::Receiver<Envelope> {
        let (new_tx, new_rx) = mpsc::channel(256);
        let old_rx = std::mem::replace(&mut self.boot_rx, new_rx);
        // Keep the new_tx in sync — but we don't need it since boot_tx is already cloned
        drop(new_tx);
        old_rx
    }

    /// Start listening for connections. This runs forever.
    pub async fn listen(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Remove stale socket file if it exists
        if self.sock_path.exists() {
            std::fs::remove_file(&self.sock_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = self.sock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.sock_path)?;
        tracing::info!(path = %self.sock_path.display(), "IPC router listening");

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let peers = Arc::clone(&self.peers);
                    let boot_tx = self.boot_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, peers, boot_tx).await {
                            tracing::warn!("Connection handler error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

    /// Send a message to a specific peer by identity.
    pub async fn send_to(&self, identity: &str, msg: Envelope) -> Result<(), String> {
        let peers = self.peers.read().await;
        if let Some(peer) = peers.get(identity) {
            peer.tx
                .send(msg)
                .await
                .map_err(|e| format!("Failed to send to {}: {}", identity, e))
        } else {
            Err(format!("Peer '{}' not connected", identity))
        }
    }

    /// Broadcast a message to all connected peers.
    pub async fn broadcast(&self, msg: Envelope) {
        let peers = self.peers.read().await;
        for (identity, peer) in peers.iter() {
            if let Err(e) = peer.tx.send(msg.clone()).await {
                tracing::warn!(peer = %identity, "Broadcast send failed: {}", e);
            }
        }
    }

    /// Get the list of currently connected peer identities.
    pub async fn connected_peers(&self) -> Vec<String> {
        let peers = self.peers.read().await;
        peers.keys().cloned().collect()
    }

    /// Remove a peer from the routing table.
    pub async fn remove_peer(&self, identity: &str) {
        let mut peers = self.peers.write().await;
        if peers.remove(identity).is_some() {
            tracing::info!(peer = %identity, "Peer disconnected");
        }
    }

    pub async fn send(&self, msg: Envelope) -> Result<(), String> {
        let dest = msg.to.clone();
        self.send_to(&dest, msg).await
    }
}

/// Handle a single connection. The first message must be a Hello handshake
/// to register the peer identity.
async fn handle_connection(
    stream: UnixStream,
    peers: Arc<RwLock<HashMap<String, PeerHandle>>>,
    boot_tx: mpsc::Sender<Envelope>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut read_half, write_half) = stream.into_split();

    // Wait for the first message to identify the peer
    let first_envelope = wire::read_envelope(&mut read_half).await?;
    let identity = first_envelope.from.clone();

    tracing::info!(peer = %identity, "New peer connected");

    // Create a channel for outgoing messages to this peer
    let (tx, mut rx) = mpsc::channel::<Envelope>(64);

    // Register the peer
    {
        let mut peers_w = peers.write().await;
        peers_w.insert(
            identity.clone(),
            PeerHandle {
                identity: identity.clone(),
                tx,
            },
        );
    }

    // Forward the first message (likely Hello) to boot
    if let Err(e) = boot_tx.send(first_envelope).await {
        tracing::warn!("Failed to forward Hello to boot: {}", e);
    }

    let identity_for_writer = identity.clone();
    let identity_for_reader = identity.clone();
    let peers_for_reader = Arc::clone(&peers);

    // Writer task: take messages from the channel and write to the socket
    let writer_handle = tokio::spawn(async move {
        let mut writer = write_half;
        while let Some(msg) = rx.recv().await {
            if let Err(e) = wire::write_envelope(&mut writer, &msg).await {
                tracing::warn!(peer = %identity_for_writer, "Write error: {}", e);
                break;
            }
        }
    });

    // Reader task: read messages from the socket and route them
    let reader_handle = tokio::spawn(async move {
        let identity = identity_for_reader;
        let peers = peers_for_reader;

        loop {
            match wire::read_envelope(&mut read_half).await {
                Ok(envelope) => {
                    let dest = envelope.to.clone();

                    if dest == "boot" {
                        // Message addressed to boot — forward to the kernel
                        if let Err(e) = boot_tx.send(envelope).await {
                            tracing::warn!("Failed to forward to boot: {}", e);
                            break;
                        }
                    } else {
                        // Route to another peer
                        let peers_r = peers.read().await;
                        if let Some(peer) = peers_r.get(&dest) {
                            if let Err(e) = peer.tx.send(envelope).await {
                                tracing::warn!(
                                    from = %identity,
                                    to = %dest,
                                    "Route failed: {}", e
                                );
                            }
                        } else {
                            tracing::warn!(
                                from = %identity,
                                to = %dest,
                                "Route failed: destination not connected"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::info!(peer = %identity, "Read error (disconnected?): {}", e);
                    break;
                }
            }
        }

        // Cleanup
        let mut peers_w = peers.write().await;
        peers_w.remove(&identity);
        tracing::info!(peer = %identity, "Peer reader finished");
    });

    // Wait for either task to finish, then abort the other
    tokio::select! {
        _ = writer_handle => {},
        _ = reader_handle => {},
    }

    // Ensure cleanup
    peers.write().await.remove(&identity);

    Ok(())
}
