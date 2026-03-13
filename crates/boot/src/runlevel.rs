//! Runlevel state machine.
//!
//! Defines the system run levels and valid transitions between them.
//! See plan §2.7 for the full specification.

use serde::{Deserialize, Serialize};

/// System run levels, modeled after Linux systemd targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Runlevel {
    /// Level 0 — system halted
    Halt = 0,
    /// Level 1 — safe mode (core agent only, all plugins disabled)
    Safe = 1,
    /// Level 2 — normal operation (default)
    Normal = 2,
    /// Level 3 — evolution mode (self-modification allowed)
    Evolve = 3,
}

impl Runlevel {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Halt),
            1 => Some(Self::Safe),
            2 => Some(Self::Normal),
            3 => Some(Self::Evolve),
            _ => None,
        }
    }
}

/// Reason for a runlevel transition.
#[derive(Debug, Clone)]
pub struct TransitionReason {
    pub description: String,
    pub automatic: bool,
}

/// Check whether a transition from `from` to `to` is valid.
///
/// Transition rules (from plan §2.7):
/// - normal → safe:  consecutive crashes after rollback
/// - normal → evolve: system idle + resource usage < 50%
/// - evolve → normal: modification complete or resources low
/// - any    → halt:   human command or unrecoverable error
/// - safe   → normal: human confirmation or auto-recovery
pub fn is_valid_transition(from: Runlevel, to: Runlevel) -> bool {
    use Runlevel::*;
    matches!(
        (from, to),
        // Any level can go to halt
        (_, Halt)
        // Safe → Normal (recovery)
        | (Safe, Normal)
        // Normal → Safe (degradation)
        | (Normal, Safe)
        // Normal ↔ Evolve
        | (Normal, Evolve)
        | (Evolve, Normal)
    )
}

/// Manages the current system runlevel and tracks transitions.
pub struct RunlevelManager {
    current: Runlevel,
    consecutive_crashes: u32,
}

impl RunlevelManager {
    pub fn new() -> Self {
        Self {
            current: Runlevel::Normal,
            consecutive_crashes: 0,
        }
    }

    pub fn current(&self) -> Runlevel {
        self.current
    }

    /// Attempt a runlevel transition. Returns `Err` if the transition is invalid.
    pub fn transition(
        &mut self,
        to: Runlevel,
        reason: TransitionReason,
    ) -> Result<Runlevel, String> {
        if !is_valid_transition(self.current, to) {
            return Err(format!(
                "Invalid runlevel transition: {:?} → {:?} (reason: {})",
                self.current, to, reason.description
            ));
        }

        let from = self.current;
        self.current = to;

        // Reset crash counter on successful recovery
        if to == Runlevel::Normal {
            self.consecutive_crashes = 0;
        }

        tracing::info!(
            from = ?from,
            to = ?to,
            reason = %reason.description,
            automatic = reason.automatic,
            "Runlevel transition"
        );

        Ok(from)
    }

    /// Record a crash event. If consecutive crashes exceed threshold,
    /// suggest transition to safe mode.
    pub fn record_crash(&mut self) -> Option<Runlevel> {
        self.consecutive_crashes += 1;
        if self.consecutive_crashes >= 2 && self.current == Runlevel::Normal {
            Some(Runlevel::Safe)
        } else {
            None
        }
    }
}
