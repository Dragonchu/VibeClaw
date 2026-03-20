//! IPC message types for the Reloopy system.
//!
//! Boot only hard-codes the **core** message types it needs to understand.
//! All other messages are treated as opaque JSON payloads and routed by `from`/`to` fields.

use serde::{Deserialize, Serialize};
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Message envelope — every IPC message is wrapped in this
// ---------------------------------------------------------------------------

/// The wire-level envelope for all IPC messages.
///
/// Boot inspects `to` for routing and `msg_type` to decide if it should
/// handle the message itself or forward it opaquely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Sender identity (e.g. "peripheral", "compiler", "judge", "audit", "boot")
    pub from: String,
    /// Destination identity
    pub to: String,
    /// Message type tag (e.g. "Hello", "LeaseRenew", "SubmitUpdate", …)
    pub msg_type: String,
    /// Unique message id for request-response correlation
    pub id: String,
    /// Arbitrary JSON payload — Boot only parses this for core message types
    pub payload: serde_json::Value,
    /// File descriptors attached out-of-band via SCM_RIGHTS (not serialized)
    #[serde(skip, default)]
    pub fds: Vec<Arc<OwnedFd>>,
}

// ---------------------------------------------------------------------------
// Core message types — Boot understands these
// ---------------------------------------------------------------------------

/// Handshake: Peripheral/Service → Boot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: String,
    pub capabilities: serde_json::Value,
}

/// Handshake response: Boot → Peripheral/Service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub accepted_capabilities: serde_json::Value,
    pub runlevel: u8,
}

/// Lease renewal: Any process → Boot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRenew {
    pub health: HealthReport,
}

/// Health report piggy-backed on lease renewal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    /// Current runlevel the process thinks it's in
    pub runlevel: u8,
    /// Memory usage in bytes
    pub memory_bytes: u64,
    /// CPU usage percentage (0.0 – 100.0)
    pub cpu_percent: f64,
    /// Number of tasks processed since last report
    pub tasks_processed: u64,
}

/// Lease acknowledgement: Boot → process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseAck {
    /// Absolute deadline (ms since UNIX epoch) by which the next renewal must arrive
    pub next_deadline_ms: u64,
}

/// Runlevel change notification: Boot → all processes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunlevelChange {
    pub from: u8,
    pub to: u8,
    pub reason: String,
}

/// Graceful shutdown command: Boot → process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shutdown {
    pub reason: String,
    /// Grace period in milliseconds before force-kill
    pub grace_ms: u64,
}

/// Prepare for listener handoff: Boot → Peripheral
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareHandoff;

/// Listener handoff ready: Peripheral → Boot (fd attached via SCM_RIGHTS)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffReady;

// ---------------------------------------------------------------------------
// Update loop message types (Phase 2)
// ---------------------------------------------------------------------------

/// Peripheral → Boot: submit source code for a new version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitUpdate {
    pub source_path: String,
}

/// Boot → Compiler: request compilation of a new version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileRequest {
    pub version: String,
    pub source_path: String,
    pub output_path: String,
}

/// Compiler → Boot: compilation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileResult {
    pub version: String,
    pub success: bool,
    pub binary_path: Option<String>,
    pub errors: Option<String>,
}

/// Boot → Peripheral: update rejected with structured feedback
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateRejected {
    pub version: String,
    pub reason: String,
    pub errors: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scores: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub allows_patch_retry: bool,
}

/// Boot → Peripheral: update accepted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAccepted {
    pub version: String,
}

// ---------------------------------------------------------------------------
// Judge system message types (Phase 3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRequest {
    pub version: String,
    pub binary_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantResult {
    pub test_id: String,
    pub passed: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionScore {
    pub name: String,
    pub score: f64,
    pub min_threshold: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestVerdict {
    Pass,
    SoftFail,
    HardFail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub version: String,
    pub verdict: TestVerdict,
    pub invariant_results: Vec<InvariantResult>,
    pub dimension_scores: Vec<DimensionScore>,
    pub overall_score: f64,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbationStarted {
    pub version: String,
    pub duration_secs: u64,
    pub constraints: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbationEnded {
    pub version: String,
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub timestamp: String,
    pub event: String,
    pub version: Option<String>,
    pub details: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Security & degradation message types (Phase 4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceViolationAlert {
    pub peer: String,
    pub resource: String,
    pub current_value: String,
    pub limit_value: String,
    /// "soft" or "hard"
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunlevelRequest {
    pub to: u8,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunlevelRequestResult {
    pub accepted: bool,
    pub from: u8,
    pub to: u8,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityEscalation {
    pub version: String,
    pub violations: Vec<String>,
}

// ---------------------------------------------------------------------------
// State management message types (Phase 2)
// ---------------------------------------------------------------------------

/// Get state: process → Boot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetState {
    pub key: String,
}

/// Get state response: Boot → process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStateResponse {
    pub key: String,
    pub value: serde_json::Value,
    pub schema_version: u64,
}

/// Set state: process → Boot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetState {
    pub key: String,
    pub value: serde_json::Value,
    pub schema_version: u64,
}

/// Set state acknowledgement: Boot → process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetStateAck {
    pub key: String,
    pub success: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Constitution & protocol evolution message types (Phase 5)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstitutionAmendmentProposal {
    pub amendment_type: String,
    pub target_file: String,
    pub description: String,
    pub changes: serde_json::Value,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstitutionAmendmentResult {
    pub accepted: bool,
    pub amendment_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolExtensionProposal {
    pub new_messages: serde_json::Value,
    pub breaking: bool,
    pub description: String,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolExtensionResult {
    pub accepted: bool,
    pub new_protocol_version: Option<String>,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Admin management message types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStatusResponse {
    pub runlevel: u8,
    pub current_version: Option<String>,
    pub rollback_version: Option<String>,
    pub connected_peers: Vec<String>,
    pub version_locked: bool,
    pub probation_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminListVersionsRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    pub is_current: bool,
    pub is_rollback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminListVersionsResponse {
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminVersionDetailRequest {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminVersionDetailResponse {
    pub version: String,
    pub manifest: Option<serde_json::Value>,
    pub is_current: bool,
    pub is_rollback: bool,
    pub has_binary: bool,
    pub has_source: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminCleanupVersionsRequest {
    pub keep: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminCleanupVersionsResponse {
    pub removed: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminForceRollbackRequest {
    pub reason: String,
    pub to_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminForceRollbackResponse {
    pub success: bool,
    pub rolled_back_to: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminLeaseStatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerLeaseInfo {
    pub identity: String,
    pub status: String,
    pub probation: bool,
    pub last_health: Option<HealthReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminLeaseStatusResponse {
    pub leases: Vec<PeerLeaseInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUnlockVersionRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUnlockVersionResponse {
    pub success: bool,
    pub was_locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAuditQueryRequest {
    pub event_filter: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAuditQueryResponse {
    pub entries: Vec<AuditLog>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminShutdownRequest {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminShutdownResponse {
    pub success: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Well-known message type constants
// ---------------------------------------------------------------------------

pub mod msg_types {
    pub const HELLO: &str = "Hello";
    pub const WELCOME: &str = "Welcome";
    pub const LEASE_RENEW: &str = "LeaseRenew";
    pub const LEASE_ACK: &str = "LeaseAck";
    pub const RUNLEVEL_CHANGE: &str = "RunlevelChange";
    pub const SHUTDOWN: &str = "Shutdown";
    pub const PREPARE_HANDOFF: &str = "PrepareHandoff";
    pub const HANDOFF_READY: &str = "HandoffReady";

    pub const SUBMIT_UPDATE: &str = "SubmitUpdate";
    pub const COMPILE_REQUEST: &str = "CompileRequest";
    pub const COMPILE_RESULT: &str = "CompileResult";
    pub const UPDATE_REJECTED: &str = "UpdateRejected";
    pub const UPDATE_ACCEPTED: &str = "UpdateAccepted";

    pub const GET_STATE: &str = "GetState";
    pub const GET_STATE_RESPONSE: &str = "GetStateResponse";
    pub const SET_STATE: &str = "SetState";
    pub const SET_STATE_ACK: &str = "SetStateAck";

    pub const TEST_REQUEST: &str = "TestRequest";
    pub const TEST_RESULT: &str = "TestResult";
    pub const PROBATION_STARTED: &str = "ProbationStarted";
    pub const PROBATION_ENDED: &str = "ProbationEnded";
    pub const AUDIT_LOG: &str = "AuditLog";

    pub const RESOURCE_VIOLATION: &str = "ResourceViolation";
    pub const RUNLEVEL_REQUEST: &str = "RunlevelRequest";
    pub const RUNLEVEL_REQUEST_RESULT: &str = "RunlevelRequestResult";
    pub const CAPABILITY_ESCALATION: &str = "CapabilityEscalation";

    pub const CONSTITUTION_AMENDMENT_PROPOSAL: &str = "ConstitutionAmendmentProposal";
    pub const CONSTITUTION_AMENDMENT_RESULT: &str = "ConstitutionAmendmentResult";
    pub const PROTOCOL_EXTENSION_PROPOSAL: &str = "ProtocolExtensionProposal";
    pub const PROTOCOL_EXTENSION_RESULT: &str = "ProtocolExtensionResult";

    pub const ADMIN_STATUS_REQUEST: &str = "AdminStatusRequest";
    pub const ADMIN_STATUS_RESPONSE: &str = "AdminStatusResponse";
    pub const ADMIN_LIST_VERSIONS_REQUEST: &str = "AdminListVersionsRequest";
    pub const ADMIN_LIST_VERSIONS_RESPONSE: &str = "AdminListVersionsResponse";
    pub const ADMIN_VERSION_DETAIL_REQUEST: &str = "AdminVersionDetailRequest";
    pub const ADMIN_VERSION_DETAIL_RESPONSE: &str = "AdminVersionDetailResponse";
    pub const ADMIN_CLEANUP_VERSIONS_REQUEST: &str = "AdminCleanupVersionsRequest";
    pub const ADMIN_CLEANUP_VERSIONS_RESPONSE: &str = "AdminCleanupVersionsResponse";
    pub const ADMIN_FORCE_ROLLBACK_REQUEST: &str = "AdminForceRollbackRequest";
    pub const ADMIN_FORCE_ROLLBACK_RESPONSE: &str = "AdminForceRollbackResponse";
    pub const ADMIN_LEASE_STATUS_REQUEST: &str = "AdminLeaseStatusRequest";
    pub const ADMIN_LEASE_STATUS_RESPONSE: &str = "AdminLeaseStatusResponse";
    pub const ADMIN_UNLOCK_VERSION_REQUEST: &str = "AdminUnlockVersionRequest";
    pub const ADMIN_UNLOCK_VERSION_RESPONSE: &str = "AdminUnlockVersionResponse";
    pub const ADMIN_AUDIT_QUERY_REQUEST: &str = "AdminAuditQueryRequest";
    pub const ADMIN_AUDIT_QUERY_RESPONSE: &str = "AdminAuditQueryResponse";
    pub const ADMIN_SHUTDOWN_REQUEST: &str = "AdminShutdownRequest";
    pub const ADMIN_SHUTDOWN_RESPONSE: &str = "AdminShutdownResponse";
}

/// Check if a message type is a core type that Boot should handle itself.
pub fn is_core_message(msg_type: &str) -> bool {
    matches!(
        msg_type,
        msg_types::HELLO
            | msg_types::WELCOME
            | msg_types::LEASE_RENEW
            | msg_types::LEASE_ACK
            | msg_types::RUNLEVEL_CHANGE
            | msg_types::SHUTDOWN
            | msg_types::PREPARE_HANDOFF
            | msg_types::HANDOFF_READY
            | msg_types::SUBMIT_UPDATE
            | msg_types::COMPILE_REQUEST
            | msg_types::COMPILE_RESULT
            | msg_types::UPDATE_REJECTED
            | msg_types::UPDATE_ACCEPTED
            | msg_types::GET_STATE
            | msg_types::GET_STATE_RESPONSE
            | msg_types::SET_STATE
            | msg_types::SET_STATE_ACK
            | msg_types::TEST_REQUEST
            | msg_types::TEST_RESULT
            | msg_types::PROBATION_STARTED
            | msg_types::PROBATION_ENDED
            | msg_types::AUDIT_LOG
            | msg_types::RESOURCE_VIOLATION
            | msg_types::RUNLEVEL_REQUEST
            | msg_types::RUNLEVEL_REQUEST_RESULT
            | msg_types::CAPABILITY_ESCALATION
            | msg_types::CONSTITUTION_AMENDMENT_PROPOSAL
            | msg_types::CONSTITUTION_AMENDMENT_RESULT
            | msg_types::PROTOCOL_EXTENSION_PROPOSAL
            | msg_types::PROTOCOL_EXTENSION_RESULT
            | msg_types::ADMIN_STATUS_REQUEST
            | msg_types::ADMIN_STATUS_RESPONSE
            | msg_types::ADMIN_LIST_VERSIONS_REQUEST
            | msg_types::ADMIN_LIST_VERSIONS_RESPONSE
            | msg_types::ADMIN_VERSION_DETAIL_REQUEST
            | msg_types::ADMIN_VERSION_DETAIL_RESPONSE
            | msg_types::ADMIN_CLEANUP_VERSIONS_REQUEST
            | msg_types::ADMIN_CLEANUP_VERSIONS_RESPONSE
            | msg_types::ADMIN_FORCE_ROLLBACK_REQUEST
            | msg_types::ADMIN_FORCE_ROLLBACK_RESPONSE
            | msg_types::ADMIN_LEASE_STATUS_REQUEST
            | msg_types::ADMIN_LEASE_STATUS_RESPONSE
            | msg_types::ADMIN_UNLOCK_VERSION_REQUEST
            | msg_types::ADMIN_UNLOCK_VERSION_RESPONSE
            | msg_types::ADMIN_AUDIT_QUERY_REQUEST
            | msg_types::ADMIN_AUDIT_QUERY_RESPONSE
            | msg_types::ADMIN_SHUTDOWN_REQUEST
            | msg_types::ADMIN_SHUTDOWN_RESPONSE
    )
}
