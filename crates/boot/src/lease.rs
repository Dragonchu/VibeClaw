//! Lease-based heartbeat management.
//!
//! Each connected process must periodically renew its lease.
//! If a lease expires, Boot considers the process dead and can take action.
//!
//! See plan §2.3 for the full specification.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use loopy_ipc::messages::HealthReport;

/// Configuration for the lease system.
#[derive(Debug, Clone)]
pub struct LeaseConfig {
    /// How long a lease is valid after renewal
    pub lease_duration: Duration,
    /// Extra time after expiry before declaring the process dead
    pub grace_period: Duration,
    /// Number of consecutive missed renewals before death declaration
    pub max_renewals_missed: u32,
    /// Tighter lease duration during probation
    pub probation_lease_duration: Duration,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        Self {
            lease_duration: Duration::from_secs(10),
            grace_period: Duration::from_secs(5),
            max_renewals_missed: 3,
            probation_lease_duration: Duration::from_secs(5),
        }
    }
}

/// State of a single peer's lease.
#[derive(Debug)]
pub struct LeaseEntry {
    /// When the lease expires (without grace period)
    pub deadline: Instant,
    /// How many consecutive renewals have been missed
    pub missed_count: u32,
    /// Whether this peer is in probation mode (tighter timeouts)
    pub probation: bool,
    /// Last health report received
    pub last_health: Option<HealthReport>,
}

/// The lease manager tracks all active leases.
pub struct LeaseManager {
    config: LeaseConfig,
    leases: HashMap<String, LeaseEntry>,
}

/// What happened when we checked a lease.
#[derive(Debug, PartialEq, Eq)]
pub enum LeaseStatus {
    /// Lease is still valid
    Alive,
    /// Lease expired but within grace period
    GracePeriod,
    /// Lease expired beyond grace period (missed count below threshold)
    Expired { missed_count: u32 },
    /// Process is declared dead (missed count >= max)
    Dead,
    /// No lease found for this identity
    Unknown,
}

impl LeaseManager {
    pub fn new(config: LeaseConfig) -> Self {
        Self {
            config,
            leases: HashMap::new(),
        }
    }

    /// Register a new peer with an initial lease.
    pub fn register(&mut self, identity: String) {
        let duration = self.config.lease_duration;
        self.leases.insert(
            identity.clone(),
            LeaseEntry {
                deadline: Instant::now() + duration,
                missed_count: 0,
                probation: false,
                last_health: None,
            },
        );
        tracing::debug!(peer = %identity, "Lease registered");
    }

    /// Renew a peer's lease. Returns the next deadline as milliseconds since UNIX epoch.
    pub fn renew(&mut self, identity: &str, health: Option<HealthReport>) -> Option<u64> {
        let entry = self.leases.get_mut(identity)?;

        let duration = if entry.probation {
            self.config.probation_lease_duration
        } else {
            self.config.lease_duration
        };

        entry.deadline = Instant::now() + duration;
        entry.missed_count = 0;
        entry.last_health = health;

        // Calculate absolute deadline as UNIX timestamp milliseconds
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let deadline_unix_ms = (now_unix + duration).as_millis() as u64;

        tracing::trace!(peer = %identity, deadline_ms = deadline_unix_ms, "Lease renewed");

        Some(deadline_unix_ms)
    }

    /// Remove a peer's lease (e.g., on graceful disconnect).
    pub fn remove(&mut self, identity: &str) {
        self.leases.remove(identity);
        tracing::debug!(peer = %identity, "Lease removed");
    }

    /// Set a peer into probation mode (tighter lease timeout).
    pub fn set_probation(&mut self, identity: &str, probation: bool) {
        if let Some(entry) = self.leases.get_mut(identity) {
            entry.probation = probation;
            tracing::info!(peer = %identity, probation, "Probation state changed");
        }
    }

    /// Check the status of a specific peer's lease.
    pub fn check(&mut self, identity: &str) -> LeaseStatus {
        let now = Instant::now();
        let grace = self.config.grace_period;
        let max_missed = self.config.max_renewals_missed;

        let entry = match self.leases.get_mut(identity) {
            Some(e) => e,
            None => return LeaseStatus::Unknown,
        };

        if now < entry.deadline {
            LeaseStatus::Alive
        } else if now < entry.deadline + grace {
            entry.missed_count += 1;
            LeaseStatus::GracePeriod
        } else {
            entry.missed_count += 1;
            if entry.missed_count >= max_missed {
                LeaseStatus::Dead
            } else {
                LeaseStatus::Expired {
                    missed_count: entry.missed_count,
                }
            }
        }
    }

    /// Check all leases and return the identities of dead peers.
    pub fn check_all(&mut self) -> Vec<(String, LeaseStatus)> {
        let now = Instant::now();
        let grace = self.config.grace_period;
        let max_missed = self.config.max_renewals_missed;
        let mut results = Vec::new();

        for (identity, entry) in &mut self.leases {
            let status = if now < entry.deadline {
                LeaseStatus::Alive
            } else if now < entry.deadline + grace {
                entry.missed_count += 1;
                LeaseStatus::GracePeriod
            } else {
                entry.missed_count += 1;
                if entry.missed_count >= max_missed {
                    LeaseStatus::Dead
                } else {
                    LeaseStatus::Expired {
                        missed_count: entry.missed_count,
                    }
                }
            };

            if status != LeaseStatus::Alive {
                results.push((identity.clone(), status));
            }
        }

        results
    }

    /// Get the last health report for a peer.
    pub fn last_health(&self, identity: &str) -> Option<&HealthReport> {
        self.leases.get(identity)?.last_health.as_ref()
    }
}
