//! Health monitoring for the Forge node.
//!
//! Tracks four categories of health:
//! - **Self-health**: Forge process itself (memory, goroutine count, etc.)
//! - **Node-health**: Host resources (CPU, memory, disk)
//! - **Workload-health**: VM and container health
//! - **Control-health**: Connection to control plane (Raft, projection staleness)
//!
//! ## Trait boundaries
//!
//! The `HealthChecker` trait allows each health category to be implemented
//! independently. The `HealthAggregator` combines all checkers into a
//! single node health status.

use serde::{Deserialize, Serialize};

/// Aggregate health status for this node.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// A single health check result.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HealthCheckResult {
    pub category: String,
    pub status: HealthStatus,
    pub message: Option<String>,
}

/// Trait for health check providers.
pub trait HealthChecker: Send + Sync {
    /// Run a health check and return the result.
    fn check(&self) -> HealthCheckResult;
}

/// Health check result for the node.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeHealth {
    pub status: HealthStatus,
    pub uptime_secs: u64,
    pub vm_count: u32,
    pub checks: Vec<HealthCheckResult>,
}

impl NodeHealth {
    pub fn healthy(uptime_secs: u64, vm_count: u32) -> Self {
        Self {
            status: HealthStatus::Healthy,
            uptime_secs,
            vm_count,
            checks: vec![],
        }
    }

    /// Aggregate health from individual checks.
    /// If any check is Unhealthy, the node is Unhealthy.
    /// If any check is Degraded and none is Unhealthy, the node is Degraded.
    /// Otherwise, the node is Healthy.
    pub fn aggregate(uptime_secs: u64, vm_count: u32, checks: Vec<HealthCheckResult>) -> Self {
        let status = if checks.iter().any(|c| c.status == HealthStatus::Unhealthy) {
            HealthStatus::Unhealthy
        } else if checks.iter().any(|c| c.status == HealthStatus::Degraded) {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };

        Self {
            status,
            uptime_secs,
            vm_count,
            checks,
        }
    }
}

/// Self-health checker (Forge process).
pub struct SelfHealthChecker;

impl HealthChecker for SelfHealthChecker {
    fn check(&self) -> HealthCheckResult {
        HealthCheckResult {
            category: "self".to_string(),
            status: HealthStatus::Healthy,
            message: None,
        }
    }
}

/// Node-health checker (host resources).
pub struct NodeHealthChecker;

impl HealthChecker for NodeHealthChecker {
    fn check(&self) -> HealthCheckResult {
        HealthCheckResult {
            category: "node".to_string(),
            status: HealthStatus::Healthy,
            message: None,
        }
    }
}

/// Control-health checker (Raft/projection).
pub struct ControlHealthChecker;

impl HealthChecker for ControlHealthChecker {
    fn check(&self) -> HealthCheckResult {
        // In bootstrap mode (no Raft), control health is always healthy.
        HealthCheckResult {
            category: "control".to_string(),
            status: HealthStatus::Healthy,
            message: Some("bootstrap mode (no Raft)".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_serializes() {
        let h = NodeHealth::healthy(100, 3);
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"healthy\""));
    }

    #[test]
    fn aggregate_healthy() {
        let checks = vec![
            HealthCheckResult {
                category: "self".into(),
                status: HealthStatus::Healthy,
                message: None,
            },
            HealthCheckResult {
                category: "node".into(),
                status: HealthStatus::Healthy,
                message: None,
            },
        ];
        let health = NodeHealth::aggregate(100, 2, checks);
        assert_eq!(health.status, HealthStatus::Healthy);
    }

    #[test]
    fn aggregate_degraded() {
        let checks = vec![
            HealthCheckResult {
                category: "self".into(),
                status: HealthStatus::Healthy,
                message: None,
            },
            HealthCheckResult {
                category: "control".into(),
                status: HealthStatus::Degraded,
                message: Some("projection stale".into()),
            },
        ];
        let health = NodeHealth::aggregate(100, 2, checks);
        assert_eq!(health.status, HealthStatus::Degraded);
    }

    #[test]
    fn aggregate_unhealthy_wins() {
        let checks = vec![
            HealthCheckResult {
                category: "self".into(),
                status: HealthStatus::Degraded,
                message: None,
            },
            HealthCheckResult {
                category: "node".into(),
                status: HealthStatus::Unhealthy,
                message: Some("disk full".into()),
            },
        ];
        let health = NodeHealth::aggregate(100, 0, checks);
        assert_eq!(health.status, HealthStatus::Unhealthy);
    }

    #[test]
    fn self_health_checker() {
        let checker = SelfHealthChecker;
        let result = checker.check();
        assert_eq!(result.status, HealthStatus::Healthy);
        assert_eq!(result.category, "self");
    }

    #[test]
    fn control_health_bootstrap() {
        let checker = ControlHealthChecker;
        let result = checker.check();
        assert_eq!(result.status, HealthStatus::Healthy);
        assert!(result.message.unwrap().contains("bootstrap"));
    }
}
