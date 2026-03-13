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
use crate::state::{MigrationTransaction, StateStore};
use crate::version::VersionManager;
use loopy_ipc::messages::{self, Envelope, LeaseAck, Welcome, msg_types};

#[derive(Debug, Clone)]
pub struct BootConfig {
    pub base_dir: PathBuf,
    pub sock_path: PathBuf,
    pub lease_config: LeaseConfig,
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

struct CapabilityRegistry;

impl CapabilityRegistry {
    fn required_capabilities(role: &str) -> Option<&'static [&'static str]> {
        match role {
            "compiler" => Some(&["compile"]),
            "judge" => Some(&["test", "score"]),
            "audit" => Some(&["log_write", "log_query"]),
            "peripheral" => Some(&["agent"]),
            _ => None,
        }
    }

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

pub struct Microkernel {
    config: BootConfig,
    lease_manager: LeaseManager,
    runlevel_manager: RunlevelManager,
    version_manager: VersionManager,
    state_store: StateStore,
}

impl Microkernel {
    pub fn new(config: BootConfig) -> Self {
        let lease_manager = LeaseManager::new(config.lease_config.clone());
        let runlevel_manager = RunlevelManager::new();
        let version_manager = VersionManager::new(&config.base_dir);
        let state_store = StateStore::new(&config.base_dir);
        Self {
            config,
            lease_manager,
            runlevel_manager,
            version_manager,
            state_store,
        }
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&self.config.base_dir)?;
        self.state_store.init().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        let mut router = IpcRouter::new(self.config.sock_path.clone());
        let mut boot_rx = router.take_boot_rx();

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
            current_version = ?self.version_manager.current_version(),
            "Boot microkernel ready"
        );

        let mut lease_tick = tokio::time::interval(self.config.lease_check_interval);

        loop {
            tokio::select! {
                Some(envelope) = boot_rx.recv() => {
                    self.handle_message(envelope, &router_ref).await;
                }
                _ = lease_tick.tick() => {
                    self.check_leases(&router_ref).await;
                }
            }
        }
    }

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
            msg_types::SUBMIT_UPDATE => {
                self.handle_submit_update(envelope, router).await;
            }
            msg_types::COMPILE_RESULT => {
                self.handle_compile_result(envelope, router).await;
            }
            msg_types::GET_STATE => {
                self.handle_get_state(envelope, router).await;
            }
            msg_types::SET_STATE => {
                self.handle_set_state(envelope, router).await;
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

    async fn handle_hello(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = &envelope.from;

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

        if let Err(reason) = CapabilityRegistry::validate(from, &hello.capabilities) {
            tracing::warn!(peer = %from, "Capability validation failed: {}", reason);
            router.remove_peer(from).await;
            return;
        }

        self.lease_manager.register(from.clone());

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

    async fn handle_lease_renew(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = &envelope.from;

        let health = serde_json::from_value::<messages::LeaseRenew>(envelope.payload.clone())
            .ok()
            .map(|lr| lr.health);

        let next_deadline = match self.lease_manager.renew(from, health) {
            Some(d) => d,
            None => {
                tracing::warn!(peer = %from, "LeaseRenew from unregistered peer");
                return;
            }
        };

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

    async fn handle_submit_update(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = envelope.from.clone();

        let submit: messages::SubmitUpdate = match serde_json::from_value(envelope.payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(peer = %from, "Invalid SubmitUpdate payload: {}", e);
                return;
            }
        };

        tracing::info!(peer = %from, source = %submit.source_path, "Update submission received");

        if self.version_manager.is_locked() {
            let rejected = messages::UpdateRejected {
                version: String::new(),
                reason: "Version manager locked due to consecutive failures".to_string(),
                errors: None,
            };
            let response = Envelope {
                from: "boot".to_string(),
                to: from,
                msg_type: msg_types::UPDATE_REJECTED.to_string(),
                id: envelope.id,
                payload: serde_json::to_value(&rejected).unwrap_or_default(),
            };
            let _ = router.send(response).await;
            return;
        }

        let version_info = match self.version_manager.allocate_version() {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to allocate version: {}", e);
                let rejected = messages::UpdateRejected {
                    version: String::new(),
                    reason: format!("Failed to allocate version: {}", e),
                    errors: None,
                };
                let response = Envelope {
                    from: "boot".to_string(),
                    to: from,
                    msg_type: msg_types::UPDATE_REJECTED.to_string(),
                    id: envelope.id,
                    payload: serde_json::to_value(&rejected).unwrap_or_default(),
                };
                let _ = router.send(response).await;
                return;
            }
        };

        let source_path = PathBuf::from(&submit.source_path);
        if let Err(e) = self.version_manager.copy_source(&source_path, &version_info.source_dir) {
            tracing::error!("Failed to copy source: {}", e);
            let rejected = messages::UpdateRejected {
                version: version_info.version,
                reason: format!("Failed to copy source: {}", e),
                errors: None,
            };
            let response = Envelope {
                from: "boot".to_string(),
                to: from,
                msg_type: msg_types::UPDATE_REJECTED.to_string(),
                id: envelope.id,
                payload: serde_json::to_value(&rejected).unwrap_or_default(),
            };
            let _ = router.send(response).await;
            return;
        }

        let compile_req = messages::CompileRequest {
            version: version_info.version.clone(),
            source_path: version_info.source_dir.to_string_lossy().to_string(),
            output_path: version_info.dir.join("target").to_string_lossy().to_string(),
        };

        tracing::info!(
            version = %version_info.version,
            "Forwarding compile request to compiler service"
        );

        let compile_envelope = Envelope {
            from: "boot".to_string(),
            to: "compiler".to_string(),
            msg_type: msg_types::COMPILE_REQUEST.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&compile_req).unwrap_or_default(),
        };

        if let Err(e) = router.send_to("compiler", compile_envelope).await {
            tracing::error!("Failed to send compile request: {}", e);
            let rejected = messages::UpdateRejected {
                version: version_info.version,
                reason: format!("Compiler service unavailable: {}", e),
                errors: None,
            };
            let response = Envelope {
                from: "boot".to_string(),
                to: from,
                msg_type: msg_types::UPDATE_REJECTED.to_string(),
                id: String::new(),
                payload: serde_json::to_value(&rejected).unwrap_or_default(),
            };
            let _ = router.send(response).await;
        }
    }

    async fn handle_compile_result(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let result: messages::CompileResult = match serde_json::from_value(envelope.payload.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Invalid CompileResult payload: {}", e);
                return;
            }
        };

        if !result.success {
            tracing::warn!(version = %result.version, "Compilation failed");
            let locked = self.version_manager.record_failure();
            if locked {
                tracing::error!("Version manager locked after consecutive failures");
            }

            let rejected = messages::UpdateRejected {
                version: result.version,
                reason: "Compilation failed".to_string(),
                errors: result.errors,
            };
            let response = Envelope {
                from: "boot".to_string(),
                to: "peripheral".to_string(),
                msg_type: msg_types::UPDATE_REJECTED.to_string(),
                id: envelope.id,
                payload: serde_json::to_value(&rejected).unwrap_or_default(),
            };
            let _ = router.send_to("peripheral", response).await;
            return;
        }

        tracing::info!(version = %result.version, "Compilation succeeded — switching version");

        if let Some(binary_path_str) = &result.binary_path {
            let binary_path = PathBuf::from(binary_path_str);
            let version_dir = self.config.base_dir.join("peripheral").join(&result.version);
            let target_binary = version_dir.join("binary");

            if binary_path.exists() {
                if let Err(e) = std::fs::copy(&binary_path, &target_binary) {
                    tracing::error!("Failed to install binary: {}", e);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &target_binary,
                        std::fs::Permissions::from_mode(0o755),
                    );
                }
            }
        }

        let old_version = self.version_manager.current_version().unwrap_or_default();
        let new_version = &result.version;

        if !old_version.is_empty() {
            match MigrationTransaction::begin(&mut self.state_store, &old_version, new_version) {
                Ok(tx) => {
                    if let Err(e) = tx.commit() {
                        tracing::error!("Migration commit failed: {}", e);
                        let rejected = messages::UpdateRejected {
                            version: new_version.to_string(),
                            reason: format!("State migration failed: {}", e),
                            errors: None,
                        };
                        let response = Envelope {
                            from: "boot".to_string(),
                            to: "peripheral".to_string(),
                            msg_type: msg_types::UPDATE_REJECTED.to_string(),
                            id: envelope.id,
                            payload: serde_json::to_value(&rejected).unwrap_or_default(),
                        };
                        let _ = router.send_to("peripheral", response).await;
                        return;
                    }
                }
                Err(e) => {
                    tracing::error!("Migration begin failed: {}", e);
                }
            }
        }

        match self.version_manager.switch_to(new_version) {
            Ok(old) => {
                tracing::info!(
                    new_version = %new_version,
                    old_version = %old,
                    "Version switch complete"
                );
            }
            Err(e) => {
                tracing::error!("Version switch failed: {}", e);
                let rejected = messages::UpdateRejected {
                    version: new_version.to_string(),
                    reason: format!("Version switch failed: {}", e),
                    errors: None,
                };
                let response = Envelope {
                    from: "boot".to_string(),
                    to: "peripheral".to_string(),
                    msg_type: msg_types::UPDATE_REJECTED.to_string(),
                    id: envelope.id,
                    payload: serde_json::to_value(&rejected).unwrap_or_default(),
                };
                let _ = router.send_to("peripheral", response).await;
                return;
            }
        }

        let accepted = messages::UpdateAccepted {
            version: new_version.to_string(),
        };
        let response = Envelope {
            from: "boot".to_string(),
            to: "peripheral".to_string(),
            msg_type: msg_types::UPDATE_ACCEPTED.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&accepted).unwrap_or_default(),
        };
        let _ = router.send_to("peripheral", response).await;
    }

    async fn handle_get_state(&self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = envelope.from.clone();

        let request: messages::GetState = match serde_json::from_value(envelope.payload.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(peer = %from, "Invalid GetState payload: {}", e);
                return;
            }
        };

        let (value, schema_version) = match self.state_store.get(&request.key) {
            Some(entry) => (entry.value.clone(), entry.schema_version),
            None => (serde_json::Value::Null, 0),
        };

        let resp = messages::GetStateResponse {
            key: request.key,
            value,
            schema_version,
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::GET_STATE_RESPONSE.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&resp).unwrap_or_default(),
        };

        if let Err(e) = router.send_to(&from, response).await {
            tracing::warn!(peer = %from, "Failed to send GetStateResponse: {}", e);
        }
    }

    async fn handle_set_state(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = envelope.from.clone();

        let request: messages::SetState = match serde_json::from_value(envelope.payload.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(peer = %from, "Invalid SetState payload: {}", e);
                return;
            }
        };

        let result = self.state_store.set(&request.key, request.value, request.schema_version);

        let ack = messages::SetStateAck {
            key: request.key,
            success: result.is_ok(),
            error: result.err(),
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::SET_STATE_ACK.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&ack).unwrap_or_default(),
        };

        if let Err(e) = router.send_to(&from, response).await {
            tracing::warn!(peer = %from, "Failed to send SetStateAck: {}", e);
        }
    }

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
