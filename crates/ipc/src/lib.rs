//! Shared IPC types and wire format for the Reloopy system.
//!
//! This crate contains:
//! - The `Envelope` message wrapper
//! - Core message types (Hello, Welcome, LeaseRenew, LeaseAck, etc.)
//! - Wire format read/write functions (length-prefixed JSON)
//!
//! These are the **stable** primitives that all Reloopy processes share.
//! Extended/dynamic message types are NOT defined here — they flow as
//! opaque `serde_json::Value` payloads inside `Envelope`.

pub mod log_err;
pub mod messages;
pub mod wire;

pub use log_err::{to_json_value, LogErr};
