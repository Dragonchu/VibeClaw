//! IPC router actor over Unix Domain Sockets.
//!
//! The router is structured as an actor: it exclusively owns the peer routing
//! table and processes commands through a message channel. Callers interact
//! with the router through the cheaply-clonable [`RouterHandle`].
//!
//! Design principles (see plan.md §2.1):
//! - **Who consumes, who owns**: the boot message channel (`boot_tx`/`boot_rx`)
//!   is created by the kernel; the router only receives `boot_tx` for forwarding.
//! - **Single source of truth**: the peer table is a plain `HashMap` owned
//!   solely by [`RouterActor`] — no `Arc<RwLock<…>>`.
//!
//! Wire format: `[4-byte big-endian length][JSON bytes]`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use reloopy_ipc::messages::Envelope;
use reloopy_ipc::wire;

// ---------------------------------------------------------------------------
// Peer handle (internal to the actor)
// ---------------------------------------------------------------------------

/// A handle representing a connected peer.
#[derive(Debug)]
struct PeerHandle {
    /// Peer identity (e.g. "peripheral", "compiler", "judge", "audit")
    identity: String,
    /// Channel to send messages to this peer
    tx: mpsc::Sender<Envelope>,
}

// ---------------------------------------------------------------------------
// Router commands — the actor's mailbox message type
// ---------------------------------------------------------------------------

/// Commands sent to the router actor through [`RouterHandle`].
enum RouterCommand {
    RegisterPeer {
        identity: String,
        tx: mpsc::Sender<Envelope>,
    },
    RemovePeer {
        identity: String,
    },
    SendTo {
        identity: String,
        msg: Envelope,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Broadcast {
        msg: Envelope,
    },
    ConnectedPeers {
        reply: oneshot::Sender<Vec<String>>,
    },
}

// ---------------------------------------------------------------------------
// RouterHandle — cheaply-clonable command sender
// ---------------------------------------------------------------------------

/// A cheaply-clonable handle for sending commands to the router actor.
///
/// All public routing operations go through this handle, which serialises
/// them into the actor's command channel.
#[derive(Clone)]
pub struct RouterHandle {
    cmd_tx: mpsc::Sender<RouterCommand>,
}

impl RouterHandle {
    /// Send a message to a specific peer by identity.
    pub async fn send_to(&self, identity: &str, msg: Envelope) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(RouterCommand::SendTo {
                identity: identity.to_string(),
                msg,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Router actor stopped".to_string())?;
        reply_rx
            .await
            .map_err(|_| "Router reply dropped".to_string())?
    }

    /// Send a message, routing based on the envelope's `to` field.
    pub async fn send(&self, msg: Envelope) -> Result<(), String> {
        let dest = msg.to.clone();
        self.send_to(&dest, msg).await
    }

    /// Broadcast a message to all connected peers.
    pub async fn broadcast(&self, msg: Envelope) {
        let _ = self.cmd_tx.send(RouterCommand::Broadcast { msg }).await;
    }

    /// Get the list of currently connected peer identities.
    pub async fn connected_peers(&self) -> Vec<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(RouterCommand::ConnectedPeers { reply: reply_tx })
            .await
            .is_err()
        {
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Remove a peer from the routing table.
    pub async fn remove_peer(&self, identity: &str) {
        let _ = self
            .cmd_tx
            .send(RouterCommand::RemovePeer {
                identity: identity.to_string(),
            })
            .await;
    }

    /// Register a new peer with the router actor.
    async fn register_peer(&self, identity: String, tx: mpsc::Sender<Envelope>) {
        let _ = self
            .cmd_tx
            .send(RouterCommand::RegisterPeer { identity, tx })
            .await;
    }
}

// ---------------------------------------------------------------------------
// RouterActor — single owner of the peer routing table
// ---------------------------------------------------------------------------

/// The router actor. Exclusively owns the peer routing table and the UDS
/// listener. Created via [`RouterActor::new()`], which also returns a
/// [`RouterHandle`].
pub struct RouterActor {
    /// Connected peers keyed by identity — sole owner, no Arc/RwLock
    peers: HashMap<String, PeerHandle>,
    /// Command mailbox
    cmd_rx: mpsc::Receiver<RouterCommand>,
    /// Sender for messages addressed to "boot", passed to connection handlers
    boot_tx: mpsc::Sender<Envelope>,
    /// Path to the Unix Domain Socket
    sock_path: PathBuf,
    /// Handle clone for spawning connection handlers
    handle: RouterHandle,
}

impl RouterActor {
    /// Create a new router actor bound to the given socket path.
    ///
    /// `boot_tx` is the sender side of the channel owned by the kernel;
    /// the kernel keeps the receiver from birth.
    pub fn new(sock_path: PathBuf, boot_tx: mpsc::Sender<Envelope>) -> (Self, RouterHandle) {
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let handle = RouterHandle { cmd_tx };
        let actor = Self {
            peers: HashMap::new(),
            cmd_rx,
            boot_tx,
            sock_path,
            handle: handle.clone(),
        };
        (actor, handle)
    }

    /// Get the path to the socket file.
    pub fn sock_path(&self) -> &Path {
        &self.sock_path
    }

    /// Run the actor event loop. Listens for new connections and processes
    /// routing commands. This runs forever until the command channel closes.
    pub async fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Remove stale socket file if it exists
        if self.sock_path.exists() {
            std::fs::remove_file(&self.sock_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = self.sock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.sock_path)?;
        tracing::info!(path = %self.sock_path.display(), "Router actor listening");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            let boot_tx = self.boot_tx.clone();
                            let handle = self.handle.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, boot_tx, handle).await {
                                    tracing::warn!("Connection handler error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Accept error: {}", e);
                        }
                    }
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    self.handle_command(cmd).await;
                }
                else => break,
            }
        }

        Ok(())
    }

    async fn handle_command(&mut self, cmd: RouterCommand) {
        match cmd {
            RouterCommand::RegisterPeer { identity, tx } => {
                self.peers.insert(
                    identity.clone(),
                    PeerHandle {
                        identity: identity.clone(),
                        tx,
                    },
                );
                tracing::debug!(peer = %identity, "Peer registered in routing table");
            }
            RouterCommand::RemovePeer { identity } => {
                if self.peers.remove(&identity).is_some() {
                    tracing::info!(peer = %identity, "Peer disconnected");
                }
            }
            RouterCommand::SendTo {
                identity,
                msg,
                reply,
            } => {
                let result = if let Some(peer) = self.peers.get(&identity) {
                    peer.tx
                        .send(msg)
                        .await
                        .map_err(|e| format!("Failed to send to {}: {}", identity, e))
                } else {
                    Err(format!("Peer '{}' not connected", identity))
                };
                let _ = reply.send(result);
            }
            RouterCommand::Broadcast { msg } => {
                for (identity, peer) in &self.peers {
                    if let Err(e) = peer.tx.send(msg.clone()).await {
                        tracing::warn!(peer = %identity, "Broadcast send failed: {}", e);
                    }
                }
            }
            RouterCommand::ConnectedPeers { reply } => {
                let _ = reply.send(self.peers.keys().cloned().collect());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

/// Handle a single connection. The first message must be a Hello handshake
/// to register the peer identity.
async fn handle_connection(
    stream: UnixStream,
    boot_tx: mpsc::Sender<Envelope>,
    handle: RouterHandle,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut read_half, write_half) = stream.into_split();

    // Wait for the first message to identify the peer
    let first_envelope = wire::read_envelope(&mut read_half).await?;
    let identity = first_envelope.from.clone();

    tracing::info!(peer = %identity, "New peer connected");

    // Create a channel for outgoing messages to this peer
    let (tx, mut rx) = mpsc::channel::<Envelope>(64);

    // Register the peer via the router actor
    handle.register_peer(identity.clone(), tx).await;

    // Forward the first message (likely Hello) to boot
    if let Err(e) = boot_tx.send(first_envelope).await {
        tracing::warn!("Failed to forward Hello to boot: {}", e);
    }

    let identity_for_writer = identity.clone();
    let identity_for_reader = identity.clone();
    let handle_for_reader = handle.clone();

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
        let handle = handle_for_reader;

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
                        // Route to another peer via the router actor
                        if let Err(e) = handle.send_to(&dest, envelope).await {
                            tracing::warn!(
                                from = %identity,
                                to = %dest,
                                "Route failed: {}", e
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
        handle.remove_peer(&identity).await;
        tracing::info!(peer = %identity, "Peer reader finished");
    });

    // Wait for either task to finish, then abort the other
    tokio::select! {
        _ = writer_handle => {},
        _ = reader_handle => {},
    }

    // Ensure cleanup (idempotent — RemovePeer on an absent identity is a no-op)
    handle.remove_peer(&identity).await;

    Ok(())
}
