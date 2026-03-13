//! Microkernel core — the main orchestrator.
//!
//! Responsibilities:
//! - Listen on Unix Domain Socket for incoming connections
//! - Handle Hello handshake and validate capabilities
//! - Route messages between peers
//! - Manage leases (heartbeat checking)
//! - Track runlevel state

use std::path::PathBuf;
use std::time::Duration;

use crate::ipc::IpcRouter;
use crate::lease::{LeaseConfig, LeaseManager, LeaseStatus};
use crate::runlevel::{RunlevelManager, TransitionReason};
use loopy_ipc::messages::{self, Envelope, LeaseAck, Welcome, msg_types};

/// Boot configuration.
#[derive(Debug, Clone)]
pub struct BootConfig {
    /// Base directory for loopy data (default: ~/.loopy)
    pub base_dir: PathBuf,
    /// Path to the Unix Domain Socket
    pub sock_path: PathBuf,
    /// Lease configuration
    pub lease_config: LeaseConfig,
    /// How often to check leases (tick interval)
    pub lease_check_interval: Duration,
}

impl Default for BootConfig {
    fn default() -> Self {
        let base_dir = dirs_home().join(".loopy");
        let sock_path = base_dir.join("loopy.sock");
        Self {
            base_dir,
            sock_path,
            lease_config: LeaseConfig::default(),
            lease_check_interval: Duration::from_secs(5),
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Required capabilities for each known service role.
/// Boot validates these during the Hello handshake.
struct CapabilityRegistry;

impl CapabilityRegistry {
    /// Returns the required capability tags for a given service role.
    fn required_capabilities(role: &str) -> Option<&'static [&'static str]> {
        match role {
            "compiler" => Some(&["compile"]),
            "judge" => Some(&["test", "score"]),
            "audit" => Some(&["log_write", "log_query"]),
            "peripheral" => Some(&["agent"]),
            _ => None,
        }
    }

    /// Validate that a Hello message's capabilities satisfy the requirements.
    fn validate(role: &str, declared: &serde_json::Value) -> Result<(), String> {
        let required = match Self::required_capabilities(role) {
            Some(caps) => caps,
            None => return Err(format!("Unknown role: {}", role)),
        };

        let declared_caps = declared
            .as_array()
            .ok_or_else(|| "capabilities must be a JSON array".to_string())?;

        let declared_strs: Vec<&str> = declared_caps.iter().filter_map(|v| v.as_str()).collect();

        for req in required {
            if !declared_strs.contains(req) {
                return Err(format!(
                    "Role '{}' requires capability '{}' but it was not declared",
                    role, req
                ));
            }
        }

        Ok(())
    }
}

/// The microkernel — ties together IPC, leases, and runlevel management.
pub struct Microkernel {
    config: BootConfig,
    lease_manager: LeaseManager,
    runlevel_manager: RunlevelManager,
}

impl Microkernel {
    pub fn new(config: BootConfig) -> Self {
        let lease_manager = LeaseManager::new(config.lease_config.clone());
        let runlevel_manager = RunlevelManager::new();
        Self {
            config,
            lease_manager,
            runlevel_manager,
        }
    }

    /// Main run loop. Starts the IPC listener and processes messages.
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Ensure base directory exists
        std::fs::create_dir_all(&self.config.base_dir)?;

        let mut router = IpcRouter::new(self.config.sock_path.clone());
        let mut boot_rx = router.take_boot_rx();

        // Spawn the IPC listener
        let router_ref = std::sync::Arc::new(router);
        let router_for_listener = std::sync::Arc::clone(&router_ref);
        tokio::spawn(async move {
            if let Err(e) = router_for_listener.listen().await {
                tracing::error!("IPC listener error: {}", e);
            }
        });

        tracing::info!(
            base_dir = %self.config.base_dir.display(),
            sock = %self.config.sock_path.display(),
            runlevel = ?self.runlevel_manager.current(),
            "Boot microkernel ready"
        );

        // Main event loop
        let mut lease_tick = tokio::time::interval(self.config.lease_check_interval);

        loop {
            tokio::select! {
                // Handle incoming messages addressed to "boot"
                Some(envelope) = boot_rx.recv() => {
                    self.handle_message(envelope, &router_ref).await;
                }
                // Periodic lease check
                _ = lease_tick.tick() => {
                    self.check_leases(&router_ref).await;
                }
            }
        }
    }

    /// Handle a message addressed to "boot".
    async fn handle_message(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        tracing::debug!(
            from = %envelope.from,
            msg_type = %envelope.msg_type,
            id = %envelope.id,
            "Handling message"
        );

        match envelope.msg_type.as_str() {
            msg_types::HELLO => {
                self.handle_hello(envelope, router).await;
            }
            msg_types::LEASE_RENEW => {
                self.handle_lease_renew(envelope, router).await;
            }
            _ => {
                if messages::is_core_message(&envelope.msg_type) {
                    tracing::warn!(
                        msg_type = %envelope.msg_type,
                        "Core message type not handled in this direction"
                    );
                } else {
                    tracing::warn!(
                        from = %envelope.from,
                        msg_type = %envelope.msg_type,
                        "Unknown message type addressed to boot"
                    );
                }
            }
        }
    }

    /// Handle a Hello handshake message.
    async fn handle_hello(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = &envelope.from;

        // Parse the Hello payload
        let hello: messages::Hello = match serde_json::from_value(envelope.payload.clone()) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(peer = %from, "Invalid Hello payload: {}", e);
                return;
            }
        };

        tracing::info!(
            peer = %from,
            protocol_version = %hello.protocol_version,
            "Hello received"
        );

        // Validate capabilities
        if let Err(reason) = CapabilityRegistry::validate(from, &hello.capabilities) {
            tracing::warn!(peer = %from, "Capability validation failed: {}", reason);
            // TODO: send a rejection message
            router.remove_peer(from).await;
            return;
        }

        // Register the lease
        self.lease_manager.register(from.clone());

        // Send Welcome response
        let welcome = Welcome {
            accepted_capabilities: hello.capabilities.clone(),
            runlevel: self.runlevel_manager.current().as_u8(),
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::WELCOME.to_string(),
            id: envelope.id.clone(),
            payload: serde_json::to_value(&welcome).unwrap_or_default(),
        };

        if let Err(e) = router.send_to(from, response).await {
            tracing::warn!(peer = %from, "Failed to send Welcome: {}", e);
        } else {
            tracing::info!(peer = %from, "Handshake complete");
        }
    }

    /// Handle a LeaseRenew message.
    async fn handle_lease_renew(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = &envelope.from;

        // Parse health report from payload
        let health = serde_json::from_value::<messages::LeaseRenew>(envelope.payload.clone())
            .ok()
            .map(|lr| lr.health);

        // Renew the lease
        let next_deadline = match self.lease_manager.renew(from, health) {
            Some(d) => d,
            None => {
                tracing::warn!(peer = %from, "LeaseRenew from unregistered peer");
                return;
            }
        };

        // Send LeaseAck
        let ack = LeaseAck {
            next_deadline_ms: next_deadline,
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::LEASE_ACK.to_string(),
            id: envelope.id.clone(),
            payload: serde_json::to_value(&ack).unwrap_or_default(),
        };

        if let Err(e) = router.send_to(from, response).await {
            tracing::warn!(peer = %from, "Failed to send LeaseAck: {}", e);
        }
    }

    /// Periodic lease health check. Handles expired and dead peers.
    async fn check_leases(&mut self, router: &std::sync::Arc<IpcRouter>) {
        let statuses = self.lease_manager.check_all();

        for (identity, status) in statuses {
            match status {
                LeaseStatus::GracePeriod => {
                    tracing::debug!(peer = %identity, "Lease in grace period");
                }
                LeaseStatus::Expired { missed_count } => {
                    tracing::warn!(
                        peer = %identity,
                        missed = missed_count,
                        "Lease expired"
                    );
                }
                LeaseStatus::Dead => {
                    tracing::error!(peer = %identity, "Peer declared dead (lease expired)");
                    router.remove_peer(&identity).await;
                    self.lease_manager.remove(&identity);

                    // Record crash and check if we need to change runlevel
                    if let Some(suggested) = self.runlevel_manager.record_crash() {
                        let reason = TransitionReason {
                            description: format!(
                                "Peer '{}' dead — consecutive failures triggered degradation",
                                identity
                            ),
                            automatic: true,
                        };
                        if let Err(e) = self.runlevel_manager.transition(suggested, reason) {
                            tracing::error!("Runlevel transition failed: {}", e);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
