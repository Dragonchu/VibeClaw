//! IPC message types for the Loopy system.
//!
//! Boot only hard-codes the **core** message types it needs to understand.
//! All other messages are treated as opaque JSON payloads and routed by `from`/`to` fields.

use serde::{Deserialize, Serialize};

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
    )
}
