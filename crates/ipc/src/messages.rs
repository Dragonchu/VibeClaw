//! IPC message types for the Reloopy system.
//!
//! Boot only hard-codes the **core** message types it needs to understand.
//! All other messages are treated as opaque JSON payloads and routed by `from`/`to` fields.

use serde::{Deserialize, Serialize};
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

fn default_attempt() -> u32 {
    1
}

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
    /// HTTP port the peer is listening on (peripheral reports this so Boot can
    /// relay it to AdminWeb for iframe embedding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_port: Option<u16>,
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

// PrepareHandoff and HandoffReady removed — hot swap no longer uses fd passing.
// Boot kills old peripheral directly and spawns new one on a random port.

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
    /// Which attempt this is for the given version (1-based).
    #[serde(default = "default_attempt")]
    pub attempt: u32,
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
    /// Which attempt just failed (1-based).
    #[serde(default)]
    pub attempt: u32,
}

/// Boot → Peripheral: update accepted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAccepted {
    pub version: String,
}

/// Boot → Peripheral: rollback context sent after Welcome when the current
/// startup is a rollback recovery.  Gives the agent structured information
/// about what failed so it can plan the next evolution attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackContext {
    /// The version that failed (e.g. "V3").
    pub from_version: String,
    /// The version that was restored (e.g. "V2").
    pub to_version: String,
    /// Machine-readable reason: "hot_swap_timeout", "spawn_failure", "user_initiated".
    pub reason: String,
    /// Compilation / test error output (may be truncated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<String>,
    /// Names of failed tests, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_tests: Vec<String>,
    /// Free-form feedback provided by the user at rollback time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_feedback: Option<String>,
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
    /// HTTP port of the currently running peripheral (if connected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peripheral_http_port: Option<u16>,
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
// Event streaming message types (AdminWeb / observability)
// ---------------------------------------------------------------------------

/// AdminWeb → Boot: subscribe to real-time event broadcasts.
///
/// Boot will forward matching events to the subscriber as they occur.
/// Use `event_filter` to select event categories (empty = all events).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscribe {
    /// Event categories to subscribe to (e.g. ["compile", "test", "audit", "runlevel"]).
    /// An empty list subscribes to all event categories.
    pub event_filter: Vec<String>,
}

/// Boot → AdminWeb: acknowledgement of event subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscribeAck {
    pub accepted: bool,
    pub subscribed_categories: Vec<String>,
}

/// Boot → subscribers: incremental compilation progress update.
///
/// Emitted while a compile is running to allow progress bars and
/// streaming log display in AdminWeb.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileProgress {
    pub version: String,
    /// Stage label (e.g. "building", "linking", "done")
    pub stage: String,
    /// Percentage complete (0–100)
    pub percent: u8,
    /// Latest compiler output line(s) (may be empty)
    pub log_line: Option<String>,
    /// Whether this is the final progress event for this version
    pub finished: bool,
    /// Which attempt this progress belongs to (1-based).
    #[serde(default = "default_attempt")]
    pub attempt: u32,
}

/// Boot → subscribers: incremental test-run progress update.
///
/// Emitted as individual tests complete, letting AdminWeb display
/// a live pass/fail tally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestProgress {
    pub version: String,
    /// Stage label (e.g. "invariants", "benchmarks", "scoring")
    pub stage: String,
    /// Number of tests completed so far
    pub completed: u32,
    /// Total number of tests (may be 0 if unknown)
    pub total: u32,
    /// Latest test id that finished (may be empty)
    pub last_test_id: Option<String>,
    /// Whether the last finished test passed
    pub last_test_passed: Option<bool>,
    /// Whether this is the final progress event for this version
    pub finished: bool,
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

    pub const EVENT_SUBSCRIBE: &str = "EventSubscribe";
    pub const EVENT_SUBSCRIBE_ACK: &str = "EventSubscribeAck";
    pub const COMPILE_PROGRESS: &str = "CompileProgress";
    pub const TEST_PROGRESS: &str = "TestProgress";

    pub const ROLLBACK_CONTEXT: &str = "RollbackContext";
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
            | msg_types::EVENT_SUBSCRIBE
            | msg_types::EVENT_SUBSCRIBE_ACK
            | msg_types::COMPILE_PROGRESS
            | msg_types::TEST_PROGRESS
            | msg_types::ROLLBACK_CONTEXT
    )
}
