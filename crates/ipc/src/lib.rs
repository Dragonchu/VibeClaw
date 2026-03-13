//! Shared IPC types and wire format for the Loopy system.
//!
//! This crate contains:
//! - The `Envelope` message wrapper
//! - Core message types (Hello, Welcome, LeaseRenew, LeaseAck, etc.)
//! - Wire format read/write functions (length-prefixed JSON)
//!
//! These are the **stable** primitives that all Loopy processes share.
//! Extended/dynamic message types are NOT defined here — they flow as
//! opaque `serde_json::Value` payloads inside `Envelope`.

pub mod messages;
pub mod wire;
