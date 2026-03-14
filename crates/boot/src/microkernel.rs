//! Microkernel core — the main orchestrator.
//!
//! Responsibilities:
//! - Listen on Unix Domain Socket for incoming connections
//! - Handle Hello handshake and validate capabilities
//! - Route messages between peers
//! - Manage leases (heartbeat checking)
//! - Track runlevel state

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::capability::CapabilityManager;
use crate::constitution::ConstitutionManager;
use crate::ipc::IpcRouter;
use crate::lease::{LeaseConfig, LeaseManager, LeaseStatus};
use crate::protocol::ProtocolManager;
use crate::resource::{ResourceLimits, ResourceMonitor, ViolationSeverity};
use crate::runlevel::{Runlevel, RunlevelManager, TransitionReason};
use crate::state::{MigrationTransaction, StateStore};
use crate::version::VersionManager;
use loopy_ipc::messages::{self, Envelope, LeaseAck, TestVerdict, Welcome, msg_types};

#[derive(Debug, Clone)]
pub struct BootConfig {
    pub base_dir: PathBuf,
    pub sock_path: PathBuf,
    pub lease_config: LeaseConfig,
    pub lease_check_interval: Duration,
    pub resource_limits: ResourceLimits,
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
            resource_limits: ResourceLimits::default(),
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

const PROBATION_DURATION_SECS: u64 = 3600;
const PROBATION_CHECK_INTERVAL_SECS: u64 = 30;
const RESOURCE_CHECK_INTERVAL_SECS: u64 = 10;

struct ProbationState {
    version: String,
    binary_path: String,
    started_at: Instant,
    duration: Duration,
    envelope_id: String,
}

const HOT_SWAP_HANDSHAKE_TIMEOUT_SECS: u64 = 60;

enum HotSwapState {
    Idle,
    WaitingForOldDisconnect {
        new_version: String,
        new_binary: PathBuf,
        old_version: String,
        initiated_at: Instant,
    },
    WaitingForNewHandshake {
        new_version: String,
        old_version: String,
        initiated_at: Instant,
        #[allow(dead_code)]
        child: Option<tokio::process::Child>,
    },
}

pub struct Microkernel {
    config: BootConfig,
    lease_manager: LeaseManager,
    runlevel_manager: RunlevelManager,
    version_manager: VersionManager,
    state_store: StateStore,
    capability_manager: CapabilityManager,
    resource_monitor: ResourceMonitor,
    probation: Option<ProbationState>,
    constitution_manager: ConstitutionManager,
    protocol_manager: ProtocolManager,
    hot_swap: HotSwapState,
}

impl Microkernel {
    pub fn new(config: BootConfig) -> Self {
        let lease_manager = LeaseManager::new(config.lease_config.clone());
        let runlevel_manager = RunlevelManager::new();
        let version_manager = VersionManager::new(&config.base_dir);
        let state_store = StateStore::new(&config.base_dir);
        let capability_manager = CapabilityManager::new(&config.base_dir);
        let resource_monitor = ResourceMonitor::new(config.resource_limits.clone());
        let constitution_manager = ConstitutionManager::new(&config.base_dir).unwrap_or_else(|e| {
            tracing::error!("Failed to init ConstitutionManager: {}", e);
            panic!("Constitution manager init failed: {}", e);
        });
        let protocol_manager = ProtocolManager::new(&config.base_dir).unwrap_or_else(|e| {
            tracing::error!("Failed to init ProtocolManager: {}", e);
            panic!("Protocol manager init failed: {}", e);
        });
        Self {
            config,
            lease_manager,
            runlevel_manager,
            version_manager,
            state_store,
            capability_manager,
            resource_monitor,
            probation: None,
            constitution_manager,
            protocol_manager,
            hot_swap: HotSwapState::Idle,
        }
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&self.config.base_dir)?;
        self.state_store
            .init()
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

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
        let mut probation_tick =
            tokio::time::interval(Duration::from_secs(PROBATION_CHECK_INTERVAL_SECS));
        let mut resource_tick =
            tokio::time::interval(Duration::from_secs(RESOURCE_CHECK_INTERVAL_SECS));
        let mut hot_swap_tick = tokio::time::interval(Duration::from_secs(2));

        loop {
            tokio::select! {
                Some(envelope) = boot_rx.recv() => {
                    self.handle_message(envelope, &router_ref).await;
                }
                _ = lease_tick.tick() => {
                    self.check_leases(&router_ref).await;
                }
                _ = probation_tick.tick() => {
                    self.check_probation(&router_ref).await;
                }
                _ = resource_tick.tick() => {
                    self.check_resource_based_transitions(&router_ref).await;
                }
                _ = hot_swap_tick.tick() => {
                    self.check_hot_swap(&router_ref).await;
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
            msg_types::TEST_RESULT => {
                self.handle_test_result(envelope, router).await;
            }
            msg_types::GET_STATE => {
                self.handle_get_state(envelope, router).await;
            }
            msg_types::SET_STATE => {
                self.handle_set_state(envelope, router).await;
            }
            msg_types::RUNLEVEL_REQUEST => {
                self.handle_runlevel_request(envelope, router).await;
            }
            msg_types::CONSTITUTION_AMENDMENT_PROPOSAL => {
                self.handle_constitution_amendment(envelope, router).await;
            }
            msg_types::PROTOCOL_EXTENSION_PROPOSAL => {
                self.handle_protocol_extension(envelope, router).await;
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

            if from == "peripheral" {
                if let HotSwapState::WaitingForNewHandshake { ref new_version, .. } = self.hot_swap {
                    tracing::info!(
                        version = %new_version,
                        "New peripheral connected — hot swap complete"
                    );
                    let nv = new_version.clone();
                    self.hot_swap = HotSwapState::Idle;
                    self.send_audit(
                        router,
                        "hot_swap_complete",
                        Some(&nv),
                        serde_json::json!({}),
                    )
                    .await;
                }
            }
        }
    }

    async fn handle_lease_renew(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let from = &envelope.from;

        let health = serde_json::from_value::<messages::LeaseRenew>(envelope.payload.clone())
            .ok()
            .map(|lr| lr.health);

        let next_deadline = match self.lease_manager.renew(from, health.clone()) {
            Some(d) => d,
            None => {
                tracing::warn!(peer = %from, "LeaseRenew from unregistered peer");
                return;
            }
        };

        if let Some(ref h) = health {
            let on_probation = self.probation.is_some() && from == "peripheral";

            let violations = self.resource_monitor.check_health(from, h, on_probation);

            for v in &violations {
                if v.severity == ViolationSeverity::Hard {
                    let alert = messages::ResourceViolationAlert {
                        peer: v.peer.clone(),
                        resource: v.resource.clone(),
                        current_value: v.current_value.clone(),
                        limit_value: v.limit_value.clone(),
                        severity: "hard".to_string(),
                    };
                    self.send_audit(
                        router,
                        "resource_violation",
                        None,
                        serde_json::to_value(&alert).unwrap_or_default(),
                    )
                    .await;
                }
            }

            if self.resource_monitor.should_degrade(from) {
                tracing::error!(
                    peer = %from,
                    "Consecutive resource hard violations — triggering degradation"
                );
                self.attempt_runlevel_transition(
                    Runlevel::Safe,
                    &format!(
                        "Peer '{}' exceeded resource hard limits consecutively",
                        from
                    ),
                    true,
                    router,
                )
                .await;
                self.resource_monitor.reset_violations(from);
            }
        }

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

    async fn handle_submit_update(
        &mut self,
        envelope: Envelope,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let from = envelope.from.clone();

        let submit: messages::SubmitUpdate = match serde_json::from_value(envelope.payload.clone())
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(peer = %from, "Invalid SubmitUpdate payload: {}", e);
                return;
            }
        };

        tracing::info!(peer = %from, source = %submit.source_path, "Update submission received");

        if self.runlevel_manager.current().is_restricted() {
            let rejected = messages::UpdateRejected {
                version: String::new(),
                reason: format!(
                    "Updates not allowed in {:?} mode",
                    self.runlevel_manager.current()
                ),
                errors: None,
                ..Default::default()
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

        if self.version_manager.is_locked() {
            let rejected = messages::UpdateRejected {
                version: String::new(),
                reason: "Version manager locked due to consecutive failures".to_string(),
                errors: None,
                ..Default::default()
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
                    ..Default::default()
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
        if let Err(e) = self
            .version_manager
            .copy_source(&source_path, &version_info.source_dir)
        {
            tracing::error!("Failed to copy source: {}", e);
            let rejected = messages::UpdateRejected {
                version: version_info.version,
                reason: format!("Failed to copy source: {}", e),
                errors: None,
                ..Default::default()
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
            output_path: version_info
                .dir
                .join("target")
                .to_string_lossy()
                .to_string(),
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
                ..Default::default()
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

    async fn handle_compile_result(
        &mut self,
        envelope: Envelope,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let result: messages::CompileResult = match serde_json::from_value(envelope.payload.clone())
        {
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
                ..Default::default()
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

        tracing::info!(version = %result.version, "Compilation succeeded — sending to judge for testing");

        if let Some(binary_path_str) = &result.binary_path {
            let binary_path = PathBuf::from(binary_path_str);
            let version_dir = self
                .config
                .base_dir
                .join("peripheral")
                .join(&result.version);
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

        let version_dir = self
            .config
            .base_dir
            .join("peripheral")
            .join(&result.version);
        let binary_path = version_dir.join("binary").to_string_lossy().to_string();

        let test_req = messages::TestRequest {
            version: result.version.clone(),
            binary_path,
        };

        let test_envelope = Envelope {
            from: "boot".to_string(),
            to: "judge".to_string(),
            msg_type: msg_types::TEST_REQUEST.to_string(),
            id: envelope.id.clone(),
            payload: serde_json::to_value(&test_req).unwrap_or_default(),
        };

        if let Err(e) = router.send_to("judge", test_envelope).await {
            tracing::warn!(
                "Judge service unavailable ({}), skipping tests — proceeding with version switch",
                e
            );
            self.finalize_version_switch(&result.version, &envelope.id, router)
                .await;
        }

        self.send_audit(
            router,
            "compilation_succeeded",
            Some(&result.version),
            serde_json::json!({}),
        )
        .await;
    }

    async fn handle_test_result(&mut self, envelope: Envelope, router: &std::sync::Arc<IpcRouter>) {
        let result: messages::TestResult = match serde_json::from_value(envelope.payload.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Invalid TestResult payload: {}", e);
                return;
            }
        };

        tracing::info!(
            version = %result.version,
            verdict = ?result.verdict,
            overall_score = result.overall_score,
            "Test result received"
        );

        match result.verdict {
            TestVerdict::Pass => {
                self.send_audit(
                    router,
                    "test_passed",
                    Some(&result.version),
                    serde_json::json!({
                        "overall_score": result.overall_score,
                        "dimension_scores": result.dimension_scores,
                    }),
                )
                .await;
                self.finalize_version_switch(&result.version, &envelope.id, router)
                    .await;
            }
            TestVerdict::SoftFail => {
                tracing::warn!(version = %result.version, "Soft fail — entering probation");

                self.probation = Some(ProbationState {
                    version: result.version.clone(),
                    binary_path: self
                        .config
                        .base_dir
                        .join("peripheral")
                        .join(&result.version)
                        .join("binary")
                        .to_string_lossy()
                        .to_string(),
                    started_at: Instant::now(),
                    duration: Duration::from_secs(PROBATION_DURATION_SECS),
                    envelope_id: envelope.id.clone(),
                });

                self.lease_manager.set_probation("peripheral", true);

                let probation_msg = messages::ProbationStarted {
                    version: result.version.clone(),
                    duration_secs: PROBATION_DURATION_SECS,
                    constraints: serde_json::json!({
                        "heartbeat_interval_secs": 5,
                        "resource_quota_multiplier": 0.5,
                    }),
                };
                let response = Envelope {
                    from: "boot".to_string(),
                    to: "peripheral".to_string(),
                    msg_type: msg_types::PROBATION_STARTED.to_string(),
                    id: envelope.id.clone(),
                    payload: serde_json::to_value(&probation_msg).unwrap_or_default(),
                };
                let _ = router.send_to("peripheral", response).await;

                self.send_audit(
                    router,
                    "probation_started",
                    Some(&result.version),
                    serde_json::json!({
                        "overall_score": result.overall_score,
                        "suggestion": result.suggestion,
                        "duration_secs": PROBATION_DURATION_SECS,
                    }),
                )
                .await;
            }
            TestVerdict::HardFail => {
                tracing::warn!(version = %result.version, "Test hard fail — rejecting update");

                let locked = self.version_manager.record_failure();
                if locked {
                    tracing::error!("Version manager locked after consecutive failures");
                }

                let failed_tests: Vec<String> = result
                    .invariant_results
                    .iter()
                    .filter(|r| !r.passed)
                    .map(|r| r.test_id.clone())
                    .collect();

                let scores_value = serde_json::to_value(&result.dimension_scores).ok();

                let rejected = messages::UpdateRejected {
                    version: result.version.clone(),
                    reason: "Test suite failed".to_string(),
                    errors: None,
                    failed_tests,
                    scores: scores_value,
                    suggestion: result.suggestion,
                    allows_patch_retry: true,
                };
                let response = Envelope {
                    from: "boot".to_string(),
                    to: "peripheral".to_string(),
                    msg_type: msg_types::UPDATE_REJECTED.to_string(),
                    id: envelope.id.clone(),
                    payload: serde_json::to_value(&rejected).unwrap_or_default(),
                };
                let _ = router.send_to("peripheral", response).await;

                self.send_audit(
                    router,
                    "update_rejected",
                    Some(&result.version),
                    serde_json::json!({
                        "reason": "Test suite hard fail",
                        "overall_score": result.overall_score,
                        "failed_tests": rejected.failed_tests,
                    }),
                )
                .await;
            }
        }
    }

    async fn finalize_version_switch(
        &mut self,
        new_version: &str,
        envelope_id: &str,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let old_version = self.version_manager.current_version().unwrap_or_default();

        if !old_version.is_empty() {
            match self
                .capability_manager
                .check_escalation(&old_version, new_version)
            {
                Ok(violations) if !violations.is_empty() => {
                    let violation_strs: Vec<String> =
                        violations.iter().map(|v| v.to_string()).collect();

                    tracing::warn!(
                        version = %new_version,
                        old_version = %old_version,
                        violations = ?violation_strs,
                        "Capability escalation detected — requires human approval"
                    );

                    let escalation = messages::CapabilityEscalation {
                        version: new_version.to_string(),
                        violations: violation_strs.clone(),
                    };

                    self.send_audit(
                        router,
                        "capability_escalation_blocked",
                        Some(new_version),
                        serde_json::to_value(&escalation).unwrap_or_default(),
                    )
                    .await;

                    let rejected = messages::UpdateRejected {
                        version: new_version.to_string(),
                        reason: "Capability escalation requires human approval".to_string(),
                        errors: Some(violation_strs.join("; ")),
                        allows_patch_retry: true,
                        ..Default::default()
                    };
                    let response = Envelope {
                        from: "boot".to_string(),
                        to: "peripheral".to_string(),
                        msg_type: msg_types::UPDATE_REJECTED.to_string(),
                        id: envelope_id.to_string(),
                        payload: serde_json::to_value(&rejected).unwrap_or_default(),
                    };
                    let _ = router.send_to("peripheral", response).await;
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        version = %new_version,
                        "Failed to check capability escalation: {} (proceeding)",
                        e
                    );
                }
                _ => {}
            }
        }

        if !old_version.is_empty() {
            match MigrationTransaction::begin(&mut self.state_store, &old_version, new_version) {
                Ok(tx) => {
                    if let Err(e) = tx.commit() {
                        tracing::error!("Migration commit failed: {}", e);
                        let rejected = messages::UpdateRejected {
                            version: new_version.to_string(),
                            reason: format!("State migration failed: {}", e),
                            errors: None,
                            ..Default::default()
                        };
                        let response = Envelope {
                            from: "boot".to_string(),
                            to: "peripheral".to_string(),
                            msg_type: msg_types::UPDATE_REJECTED.to_string(),
                            id: envelope_id.to_string(),
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
                    ..Default::default()
                };
                let response = Envelope {
                    from: "boot".to_string(),
                    to: "peripheral".to_string(),
                    msg_type: msg_types::UPDATE_REJECTED.to_string(),
                    id: envelope_id.to_string(),
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
            id: envelope_id.to_string(),
            payload: serde_json::to_value(&accepted).unwrap_or_default(),
        };
        let _ = router.send_to("peripheral", response).await;

        self.send_audit(
            router,
            "update_accepted",
            Some(new_version),
            serde_json::json!({}),
        )
        .await;

        let new_binary = self
            .config
            .base_dir
            .join("peripheral")
            .join(new_version)
            .join("binary");

        if new_binary.exists() {
            tracing::info!(
                version = %new_version,
                "Initiating hot swap — sending Shutdown to old peripheral"
            );

            let shutdown = messages::Shutdown {
                reason: format!("Hot replacement: upgrading to {}", new_version),
                grace_ms: 5000,
            };
            let shutdown_envelope = Envelope {
                from: "boot".to_string(),
                to: "peripheral".to_string(),
                msg_type: msg_types::SHUTDOWN.to_string(),
                id: String::new(),
                payload: serde_json::to_value(&shutdown).unwrap_or_default(),
            };
            let _ = router.send_to("peripheral", shutdown_envelope).await;

            self.hot_swap = HotSwapState::WaitingForOldDisconnect {
                new_version: new_version.to_string(),
                new_binary,
                old_version,
                initiated_at: Instant::now(),
            };
        } else {
            tracing::warn!(
                version = %new_version,
                "New binary not found at {:?} — skipping hot swap",
                new_binary
            );
        }
    }

    async fn check_probation(&mut self, router: &std::sync::Arc<IpcRouter>) {
        let probation = match &self.probation {
            Some(p) => p,
            None => return,
        };

        if probation.started_at.elapsed() < probation.duration {
            return;
        }

        let version = probation.version.clone();
        let binary_path = probation.binary_path.clone();
        let envelope_id = probation.envelope_id.clone();

        tracing::info!(version = %version, "Probation period expired — requesting re-evaluation");

        let test_req = messages::TestRequest {
            version: version.clone(),
            binary_path,
        };

        let test_envelope = Envelope {
            from: "boot".to_string(),
            to: "judge".to_string(),
            msg_type: msg_types::TEST_REQUEST.to_string(),
            id: format!("probation-reeval-{}", envelope_id),
            payload: serde_json::to_value(&test_req).unwrap_or_default(),
        };

        self.lease_manager.set_probation("peripheral", false);
        self.probation = None;

        if let Err(e) = router.send_to("judge", test_envelope).await {
            tracing::error!("Failed to send probation re-evaluation request: {}", e);

            let probation_msg = messages::ProbationEnded {
                version: version.clone(),
                passed: false,
                reason: format!("Judge unavailable for re-evaluation: {}", e),
            };
            let response = Envelope {
                from: "boot".to_string(),
                to: "peripheral".to_string(),
                msg_type: msg_types::PROBATION_ENDED.to_string(),
                id: envelope_id,
                payload: serde_json::to_value(&probation_msg).unwrap_or_default(),
            };
            let _ = router.send_to("peripheral", response).await;

            self.send_audit(
                router,
                "probation_failed",
                Some(&version),
                serde_json::json!({
                    "reason": "Judge unavailable for re-evaluation",
                }),
            )
            .await;
        }
    }

    async fn send_audit(
        &self,
        router: &std::sync::Arc<IpcRouter>,
        event: &str,
        version: Option<&str>,
        details: serde_json::Value,
    ) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();

        let audit = messages::AuditLog {
            timestamp,
            event: event.to_string(),
            version: version.map(|v| v.to_string()),
            details,
        };

        let envelope = Envelope {
            from: "boot".to_string(),
            to: "audit".to_string(),
            msg_type: msg_types::AUDIT_LOG.to_string(),
            id: String::new(),
            payload: serde_json::to_value(&audit).unwrap_or_default(),
        };

        if let Err(e) = router.send_to("audit", envelope).await {
            tracing::debug!("Audit service not available: {} (non-critical)", e);
        }
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

        let result = self
            .state_store
            .set(&request.key, request.value, request.schema_version);

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
                    let during_hot_swap = identity == "peripheral"
                        && matches!(self.hot_swap, HotSwapState::WaitingForOldDisconnect { .. });

                    tracing::error!(peer = %identity, during_hot_swap, "Peer declared dead (lease expired)");
                    router.remove_peer(&identity).await;
                    self.lease_manager.remove(&identity);
                    self.resource_monitor.remove_peer(&identity);

                    if during_hot_swap {
                        self.advance_hot_swap_after_disconnect(router).await;
                    } else if let Some(suggested) = self.runlevel_manager.record_crash() {
                        self.attempt_runlevel_transition(
                            suggested,
                            &format!(
                                "Peer '{}' dead — consecutive failures triggered degradation",
                                identity
                            ),
                            true,
                            router,
                        )
                        .await;
                    }
                }
                _ => {}
            }
        }
    }

    async fn advance_hot_swap_after_disconnect(&mut self, router: &std::sync::Arc<IpcRouter>) {
        let (new_version, new_binary, old_version) = match std::mem::replace(&mut self.hot_swap, HotSwapState::Idle) {
            HotSwapState::WaitingForOldDisconnect {
                new_version,
                new_binary,
                old_version,
                ..
            } => (new_version, new_binary, old_version),
            other => {
                self.hot_swap = other;
                return;
            }
        };

        tracing::info!(
            new_version = %new_version,
            binary = %new_binary.display(),
            "Old peripheral disconnected — spawning new version"
        );

        match self.spawn_peripheral(&new_binary, &new_version).await {
            Ok(child) => {
                self.hot_swap = HotSwapState::WaitingForNewHandshake {
                    new_version,
                    old_version,
                    initiated_at: Instant::now(),
                    child: Some(child),
                };
            }
            Err(e) => {
                tracing::error!("Failed to spawn new peripheral: {}", e);
                tracing::warn!("Rolling back to previous version");
                if let Err(re) = self.version_manager.rollback() {
                    tracing::error!("Rollback also failed: {}", re);
                }
                self.send_audit(
                    router,
                    "hot_swap_failed",
                    Some(&new_version),
                    serde_json::json!({ "error": e.to_string() }),
                )
                .await;
            }
        }
    }

    async fn spawn_peripheral(
        &self,
        binary_path: &PathBuf,
        version: &str,
    ) -> Result<tokio::process::Child, Box<dyn std::error::Error>> {
        let source_dir = self
            .config
            .base_dir
            .join("peripheral")
            .join(version)
            .join("source");

        let child = tokio::process::Command::new(binary_path)
            .env("LOOPY_WORKSPACE", &source_dir)
            .env(
                "LOOPY_SOCKET",
                self.config.sock_path.to_string_lossy().to_string(),
            )
            .env("RUST_LOG", "info")
            .spawn()?;

        tracing::info!(
            version = %version,
            pid = child.id().unwrap_or(0),
            "Spawned new peripheral process"
        );

        Ok(child)
    }

    async fn check_hot_swap(&mut self, router: &std::sync::Arc<IpcRouter>) {
        match &self.hot_swap {
            HotSwapState::Idle => {}
            HotSwapState::WaitingForOldDisconnect { initiated_at, .. } => {
                if initiated_at.elapsed() > Duration::from_secs(15) {
                    tracing::warn!("Old peripheral did not disconnect within timeout — forcing advance");
                    router.remove_peer("peripheral").await;
                    self.lease_manager.remove("peripheral");
                    self.advance_hot_swap_after_disconnect(router).await;
                }
            }
            HotSwapState::WaitingForNewHandshake {
                new_version,
                old_version,
                initiated_at,
                ..
            } => {
                if initiated_at.elapsed() > Duration::from_secs(HOT_SWAP_HANDSHAKE_TIMEOUT_SECS) {
                    tracing::error!(
                        new_version = %new_version,
                        "New peripheral failed to handshake within timeout — rolling back"
                    );
                    let nv = new_version.clone();
                    let ov = old_version.clone();

                    self.hot_swap = HotSwapState::Idle;

                    if let Err(e) = self.version_manager.rollback() {
                        tracing::error!("Rollback failed: {}", e);
                    } else {
                        tracing::info!(version = %ov, "Rolled back to previous version");
                        let rollback_binary = self
                            .config
                            .base_dir
                            .join("peripheral")
                            .join(&ov)
                            .join("binary");

                        if rollback_binary.exists() {
                            match self.spawn_peripheral(&rollback_binary, &ov).await {
                                Ok(_child) => {
                                    tracing::info!(version = %ov, "Spawned rollback peripheral");
                                }
                                Err(e) => {
                                    tracing::error!("Failed to spawn rollback peripheral: {}", e);
                                }
                            }
                        }
                    }

                    self.send_audit(
                        router,
                        "hot_swap_timeout_rollback",
                        Some(&nv),
                        serde_json::json!({
                            "rolled_back_to": ov,
                        }),
                    )
                    .await;
                }
            }
        }
    }

    async fn handle_runlevel_request(
        &mut self,
        envelope: Envelope,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let from = envelope.from.clone();

        let request: messages::RunlevelRequest =
            match serde_json::from_value(envelope.payload.clone()) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(peer = %from, "Invalid RunlevelRequest payload: {}", e);
                    return;
                }
            };

        let target = match Runlevel::from_u8(request.to) {
            Some(r) => r,
            None => {
                let result = messages::RunlevelRequestResult {
                    accepted: false,
                    from: self.runlevel_manager.current().as_u8(),
                    to: request.to,
                    reason: format!("Invalid runlevel: {}", request.to),
                };
                let response = Envelope {
                    from: "boot".to_string(),
                    to: from,
                    msg_type: msg_types::RUNLEVEL_REQUEST_RESULT.to_string(),
                    id: envelope.id,
                    payload: serde_json::to_value(&result).unwrap_or_default(),
                };
                let _ = router.send(response).await;
                return;
            }
        };

        let from_level = self.runlevel_manager.current();
        let transition_result = self.runlevel_manager.transition(
            target,
            TransitionReason {
                description: format!("Requested by {}: {}", from, request.reason),
                automatic: false,
            },
        );

        let (accepted, reason) = match transition_result {
            Ok(_) => (true, request.reason.clone()),
            Err(e) => (false, e),
        };

        let result = messages::RunlevelRequestResult {
            accepted,
            from: from_level.as_u8(),
            to: target.as_u8(),
            reason: reason.clone(),
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::RUNLEVEL_REQUEST_RESULT.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&result).unwrap_or_default(),
        };
        let _ = router.send(response).await;

        if accepted {
            self.broadcast_runlevel_change(from_level, target, &reason, router)
                .await;
            self.send_audit(
                router,
                "runlevel_change",
                None,
                serde_json::json!({
                    "from": from_level.as_u8(),
                    "to": target.as_u8(),
                    "reason": reason,
                    "requested_by": from,
                }),
            )
            .await;

            if target == Runlevel::Halt {
                self.initiate_shutdown(router).await;
            }
        }
    }

    async fn attempt_runlevel_transition(
        &mut self,
        to: Runlevel,
        reason: &str,
        automatic: bool,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let transition = self.runlevel_manager.transition(
            to,
            TransitionReason {
                description: reason.to_string(),
                automatic,
            },
        );

        if let Ok(old_level) = transition {
            if old_level != to {
                self.broadcast_runlevel_change(old_level, to, reason, router)
                    .await;
                self.send_audit(
                    router,
                    "runlevel_change",
                    None,
                    serde_json::json!({
                        "from": old_level.as_u8(),
                        "to": to.as_u8(),
                        "reason": reason,
                        "automatic": automatic,
                    }),
                )
                .await;

                if to == Runlevel::Halt {
                    self.initiate_shutdown(router).await;
                }
            }
        }
    }

    async fn broadcast_runlevel_change(
        &self,
        from: Runlevel,
        to: Runlevel,
        reason: &str,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let change = messages::RunlevelChange {
            from: from.as_u8(),
            to: to.as_u8(),
            reason: reason.to_string(),
        };

        let envelope = Envelope {
            from: "boot".to_string(),
            to: String::new(),
            msg_type: msg_types::RUNLEVEL_CHANGE.to_string(),
            id: String::new(),
            payload: serde_json::to_value(&change).unwrap_or_default(),
        };

        router.broadcast(envelope).await;
    }

    async fn initiate_shutdown(&self, router: &std::sync::Arc<IpcRouter>) {
        tracing::warn!("Initiating system shutdown — sending Shutdown to all peers");

        let shutdown = messages::Shutdown {
            reason: "System entering Halt runlevel".to_string(),
            grace_ms: 5000,
        };

        let envelope = Envelope {
            from: "boot".to_string(),
            to: String::new(),
            msg_type: msg_types::SHUTDOWN.to_string(),
            id: String::new(),
            payload: serde_json::to_value(&shutdown).unwrap_or_default(),
        };

        router.broadcast(envelope).await;
    }

    async fn check_resource_based_transitions(&mut self, router: &std::sync::Arc<IpcRouter>) {
        let avg_cpu = self.lease_manager.avg_cpu_percent();

        if self.runlevel_manager.should_exit_evolve(avg_cpu) {
            self.attempt_runlevel_transition(
                Runlevel::Normal,
                &format!(
                    "Resource pressure too high for evolve mode (avg CPU: {:.1}%)",
                    avg_cpu
                ),
                true,
                router,
            )
            .await;
        }
    }

    async fn handle_constitution_amendment(
        &mut self,
        envelope: Envelope,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let from = envelope.from.clone();

        let proposal: messages::ConstitutionAmendmentProposal =
            match serde_json::from_value(envelope.payload.clone()) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(peer = %from, "Invalid ConstitutionAmendmentProposal: {}", e);
                    return;
                }
            };

        tracing::info!(
            peer = %from,
            amendment_type = %proposal.amendment_type,
            target = %proposal.target_file,
            "Constitution amendment proposal received"
        );

        let result = self.constitution_manager.propose_amendment(
            &proposal.amendment_type,
            &proposal.target_file,
            &proposal.description,
            &proposal.changes,
            &proposal.signature,
        );

        let (accepted, amendment_id, reason) = match result {
            Ok(record) => (
                true,
                record.id.clone(),
                format!("Amendment {} approved", record.id),
            ),
            Err(e) => (false, String::new(), e),
        };

        let response_payload = messages::ConstitutionAmendmentResult {
            accepted,
            amendment_id: amendment_id.clone(),
            reason: reason.clone(),
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::CONSTITUTION_AMENDMENT_RESULT.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&response_payload).unwrap_or_default(),
        };

        if let Err(e) = router.send_to(&from, response).await {
            tracing::warn!(peer = %from, "Failed to send ConstitutionAmendmentResult: {}", e);
        }

        self.send_audit(
            router,
            if accepted {
                "constitution_amendment_accepted"
            } else {
                "constitution_amendment_rejected"
            },
            None,
            serde_json::json!({
                "amendment_id": amendment_id,
                "amendment_type": proposal.amendment_type,
                "target_file": proposal.target_file,
                "reason": reason,
                "proposed_by": from,
            }),
        )
        .await;
    }

    async fn handle_protocol_extension(
        &mut self,
        envelope: Envelope,
        router: &std::sync::Arc<IpcRouter>,
    ) {
        let from = envelope.from.clone();

        let proposal: messages::ProtocolExtensionProposal =
            match serde_json::from_value(envelope.payload.clone()) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(peer = %from, "Invalid ProtocolExtensionProposal: {}", e);
                    return;
                }
            };

        tracing::info!(
            peer = %from,
            breaking = proposal.breaking,
            "Protocol extension proposal received"
        );

        let sig_verifier = |data: &str, sig: &str| -> bool {
            self.constitution_manager.verify_signature(data, sig)
        };

        let result = self.protocol_manager.propose_extension(
            &proposal.new_messages,
            proposal.breaking,
            &proposal.description,
            if proposal.signature.is_some() {
                Some(&sig_verifier as &dyn Fn(&str, &str) -> bool)
            } else {
                None
            },
            proposal.signature.as_deref(),
        );

        let (accepted, new_version, reason) = match result {
            Ok(version) => (
                true,
                Some(version.clone()),
                format!("Protocol updated to {}", version),
            ),
            Err(e) => (false, None, e),
        };

        let response_payload = messages::ProtocolExtensionResult {
            accepted,
            new_protocol_version: new_version.clone(),
            reason: reason.clone(),
        };

        let response = Envelope {
            from: "boot".to_string(),
            to: from.clone(),
            msg_type: msg_types::PROTOCOL_EXTENSION_RESULT.to_string(),
            id: envelope.id,
            payload: serde_json::to_value(&response_payload).unwrap_or_default(),
        };

        if let Err(e) = router.send_to(&from, response).await {
            tracing::warn!(peer = %from, "Failed to send ProtocolExtensionResult: {}", e);
        }

        self.send_audit(
            router,
            if accepted {
                "protocol_extension_accepted"
            } else {
                "protocol_extension_rejected"
            },
            None,
            serde_json::json!({
                "new_version": new_version,
                "breaking": proposal.breaking,
                "reason": reason,
                "proposed_by": from,
            }),
        )
        .await;
    }
}
