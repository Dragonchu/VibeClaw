//! Utility trait and helpers for logging errors before discarding them.
//!
//! Instead of silently ignoring failures with `let _ = expr`, prefer:
//! - `.warn_err()` for recoverable errors worth tracking (peer disconnect, cleanup failure, etc.)
//! - `.log_err()` for serious errors that shouldn't happen in normal operation
//! - `.ok()` only for truly intentional fire-and-forget (e.g., SSE/event channels where
//!   the receiver may legitimately be gone)

/// Extension trait that logs an error before discarding it.
pub trait LogErr<T> {
    /// If `Err`, log at `warn` level and return `None`; if `Ok`, return `Some(value)`.
    fn warn_err(self) -> Option<T>;

    /// If `Err`, log at `error` level and return `None`; if `Ok`, return `Some(value)`.
    fn log_err(self) -> Option<T>;
}

impl<T, E: std::fmt::Display> LogErr<T> for Result<T, E> {
    fn warn_err(self) -> Option<T> {
        match self {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("{}", e);
                None
            }
        }
    }

    fn log_err(self) -> Option<T> {
        match self {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::error!("{}", e);
                None
            }
        }
    }
}

/// Serialize `v` to [`serde_json::Value`], logging an error and returning
/// [`serde_json::Value::Null`] if serialization fails.
///
/// Use this instead of `.unwrap_or_default()` on `serde_json::to_value` so
/// that unexpected serialization failures are never silently swallowed.
pub fn to_json_value<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or_else(|e| {
        tracing::error!(error = %e, "Failed to serialize value to JSON; sending null payload");
        serde_json::Value::Null
    })
}
