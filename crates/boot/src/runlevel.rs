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

    pub fn allows_evolution(self) -> bool {
        self == Self::Evolve
    }

    pub fn is_restricted(self) -> bool {
        self == Self::Safe || self == Self::Halt
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
        (_, Halt)
            | (Safe, Normal)
            | (Normal, Safe)
            | (Normal, Evolve)
            | (Evolve, Normal)
            | (Evolve, Safe)
    )
}

const CRASH_THRESHOLD_FOR_SAFE: u32 = 2;
const EVOLVE_CPU_THRESHOLD: f64 = 50.0;

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
        if self.current == to {
            return Ok(self.current);
        }

        if !is_valid_transition(self.current, to) {
            return Err(format!(
                "Invalid runlevel transition: {:?} → {:?} (reason: {})",
                self.current, to, reason.description
            ));
        }

        let from = self.current;
        self.current = to;

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
        if self.consecutive_crashes >= CRASH_THRESHOLD_FOR_SAFE && self.current == Runlevel::Normal
        {
            Some(Runlevel::Safe)
        } else {
            None
        }
    }

    pub fn consecutive_crashes(&self) -> u32 {
        self.consecutive_crashes
    }

    /// Check if conditions are met to enter evolve mode.
    /// Requires Normal mode + low CPU usage.
    pub fn can_enter_evolve(&self, avg_cpu_percent: f64) -> bool {
        self.current == Runlevel::Normal && avg_cpu_percent < EVOLVE_CPU_THRESHOLD
    }

    /// Check if evolve mode should be exited due to resource pressure.
    pub fn should_exit_evolve(&self, avg_cpu_percent: f64) -> bool {
        self.current == Runlevel::Evolve && avg_cpu_percent >= EVOLVE_CPU_THRESHOLD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_to_safe_after_crashes() {
        let mut mgr = RunlevelManager::new();
        assert_eq!(mgr.record_crash(), None);
        assert_eq!(mgr.record_crash(), Some(Runlevel::Safe));
    }

    #[test]
    fn transition_resets_crashes() {
        let mut mgr = RunlevelManager::new();
        mgr.record_crash();
        mgr.record_crash();
        let _ = mgr.transition(
            Runlevel::Safe,
            TransitionReason {
                description: "test".to_string(),
                automatic: true,
            },
        );
        let _ = mgr.transition(
            Runlevel::Normal,
            TransitionReason {
                description: "recovery".to_string(),
                automatic: false,
            },
        );
        assert_eq!(mgr.consecutive_crashes(), 0);
    }

    #[test]
    fn evolve_to_safe_is_valid() {
        assert!(is_valid_transition(Runlevel::Evolve, Runlevel::Safe));
    }

    #[test]
    fn safe_to_evolve_is_invalid() {
        assert!(!is_valid_transition(Runlevel::Safe, Runlevel::Evolve));
    }

    #[test]
    fn can_enter_evolve_checks_cpu() {
        let mgr = RunlevelManager::new();
        assert!(mgr.can_enter_evolve(30.0));
        assert!(!mgr.can_enter_evolve(60.0));
    }

    #[test]
    fn noop_on_same_level() {
        let mut mgr = RunlevelManager::new();
        let result = mgr.transition(
            Runlevel::Normal,
            TransitionReason {
                description: "no change".to_string(),
                automatic: false,
            },
        );
        assert!(result.is_ok());
    }
}
