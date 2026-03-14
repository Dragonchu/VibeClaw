//! Resource quota monitoring and enforcement.
//!
//! Monitors peer health reports against configured limits and triggers
//! appropriate actions (warnings, degradations).
//! See plan §2.8.

use std::collections::HashMap;

use loopy_ipc::messages::HealthReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_mb: u64,
    pub max_cpu_percent: f64,
    pub max_open_files: u64,
    pub max_disk_write_mb: u64,
    pub max_process_count: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_mb: 512,
            max_cpu_percent: 80.0,
            max_open_files: 256,
            max_disk_write_mb: 100,
            max_process_count: 4,
        }
    }
}

const SOFT_LIMIT_RATIO: f64 = 0.8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationSeverity {
    Soft,
    Hard,
}

#[derive(Debug, Clone)]
pub struct ResourceViolation {
    pub peer: String,
    pub resource: String,
    pub current_value: String,
    pub limit_value: String,
    pub severity: ViolationSeverity,
}

impl std::fmt::Display for ResourceViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.severity {
            ViolationSeverity::Soft => "SOFT",
            ViolationSeverity::Hard => "HARD",
        };
        write!(
            f,
            "[{}] {}: {} = {} (limit: {})",
            level, self.peer, self.resource, self.current_value, self.limit_value
        )
    }
}

pub struct ResourceMonitor {
    limits: ResourceLimits,
    probation_limits: ResourceLimits,
    consecutive_violations: HashMap<String, u32>,
}

const HARD_VIOLATION_THRESHOLD: u32 = 3;

impl ResourceMonitor {
    pub fn new(limits: ResourceLimits) -> Self {
        let probation_limits = ResourceLimits {
            max_memory_mb: limits.max_memory_mb / 2,
            max_cpu_percent: limits.max_cpu_percent / 2.0,
            max_open_files: limits.max_open_files / 2,
            max_disk_write_mb: limits.max_disk_write_mb / 2,
            max_process_count: limits.max_process_count.max(1) / 2,
        };

        Self {
            limits,
            probation_limits,
            consecutive_violations: HashMap::new(),
        }
    }

    pub fn check_health(
        &mut self,
        peer: &str,
        health: &HealthReport,
        on_probation: bool,
    ) -> Vec<ResourceViolation> {
        let limits = if on_probation {
            &self.probation_limits
        } else {
            &self.limits
        };

        let mut violations = Vec::new();

        let memory_mb = health.memory_bytes / (1024 * 1024);
        check_limit(
            peer,
            "memory_mb",
            memory_mb as f64,
            limits.max_memory_mb as f64,
            &mut violations,
        );

        check_limit(
            peer,
            "cpu_percent",
            health.cpu_percent,
            limits.max_cpu_percent,
            &mut violations,
        );

        if violations
            .iter()
            .any(|v| v.severity == ViolationSeverity::Hard)
        {
            let count = self
                .consecutive_violations
                .entry(peer.to_string())
                .or_insert(0);
            *count += 1;
        } else {
            self.consecutive_violations.remove(peer);
        }

        for v in &violations {
            match v.severity {
                ViolationSeverity::Soft => {
                    tracing::warn!(
                        peer = %v.peer,
                        resource = %v.resource,
                        current = %v.current_value,
                        limit = %v.limit_value,
                        "Resource soft limit exceeded"
                    );
                }
                ViolationSeverity::Hard => {
                    tracing::error!(
                        peer = %v.peer,
                        resource = %v.resource,
                        current = %v.current_value,
                        limit = %v.limit_value,
                        "Resource HARD limit exceeded"
                    );
                }
            }
        }

        violations
    }

    pub fn should_degrade(&self, peer: &str) -> bool {
        self.consecutive_violations
            .get(peer)
            .is_some_and(|&count| count >= HARD_VIOLATION_THRESHOLD)
    }

    pub fn reset_violations(&mut self, peer: &str) {
        self.consecutive_violations.remove(peer);
    }

    pub fn remove_peer(&mut self, peer: &str) {
        self.consecutive_violations.remove(peer);
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }
}

fn check_limit(
    peer: &str,
    resource: &str,
    current: f64,
    max: f64,
    violations: &mut Vec<ResourceViolation>,
) {
    let soft_limit = max * SOFT_LIMIT_RATIO;

    if current > max {
        violations.push(ResourceViolation {
            peer: peer.to_string(),
            resource: resource.to_string(),
            current_value: format!("{:.1}", current),
            limit_value: format!("{:.1}", max),
            severity: ViolationSeverity::Hard,
        });
    } else if current > soft_limit {
        violations.push(ResourceViolation {
            peer: peer.to_string(),
            resource: resource.to_string(),
            current_value: format!("{:.1}", current),
            limit_value: format!("{:.1} (soft: {:.1})", max, soft_limit),
            severity: ViolationSeverity::Soft,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_health(memory_mb: u64, cpu: f64) -> HealthReport {
        HealthReport {
            runlevel: 2,
            memory_bytes: memory_mb * 1024 * 1024,
            cpu_percent: cpu,
            tasks_processed: 0,
        }
    }

    #[test]
    fn no_violation_under_soft_limit() {
        let mut monitor = ResourceMonitor::new(ResourceLimits::default());
        let health = make_health(200, 50.0);
        let violations = monitor.check_health("test", &health, false);
        assert!(violations.is_empty());
    }

    #[test]
    fn soft_violation_between_80_and_100_percent() {
        let mut monitor = ResourceMonitor::new(ResourceLimits {
            max_memory_mb: 512,
            max_cpu_percent: 80.0,
            ..Default::default()
        });
        let health = make_health(450, 50.0);
        let violations = monitor.check_health("test", &health, false);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].severity, ViolationSeverity::Soft);
        assert_eq!(violations[0].resource, "memory_mb");
    }

    #[test]
    fn hard_violation_over_100_percent() {
        let mut monitor = ResourceMonitor::new(ResourceLimits {
            max_memory_mb: 512,
            max_cpu_percent: 80.0,
            ..Default::default()
        });
        let health = make_health(600, 90.0);
        let violations = monitor.check_health("test", &health, false);
        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .all(|v| v.severity == ViolationSeverity::Hard)
        );
    }

    #[test]
    fn degradation_after_consecutive_hard_violations() {
        let mut monitor = ResourceMonitor::new(ResourceLimits {
            max_memory_mb: 512,
            max_cpu_percent: 80.0,
            ..Default::default()
        });
        let health = make_health(600, 50.0);
        for _ in 0..HARD_VIOLATION_THRESHOLD {
            monitor.check_health("test", &health, false);
        }
        assert!(monitor.should_degrade("test"));
    }

    #[test]
    fn probation_uses_halved_limits() {
        let mut monitor = ResourceMonitor::new(ResourceLimits {
            max_memory_mb: 512,
            max_cpu_percent: 80.0,
            ..Default::default()
        });
        let health = make_health(300, 50.0);
        let violations_normal = monitor.check_health("test", &health, false);
        assert!(violations_normal.is_empty());

        let violations_probation = monitor.check_health("test", &health, true);
        assert!(!violations_probation.is_empty());
    }

    #[test]
    fn violations_reset_on_clean_report() {
        let mut monitor = ResourceMonitor::new(ResourceLimits {
            max_memory_mb: 512,
            max_cpu_percent: 80.0,
            ..Default::default()
        });
        let bad = make_health(600, 50.0);
        monitor.check_health("test", &bad, false);
        monitor.check_health("test", &bad, false);

        let good = make_health(200, 50.0);
        monitor.check_health("test", &good, false);
        assert!(!monitor.should_degrade("test"));
    }
}
