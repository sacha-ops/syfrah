//! Health monitoring for the Forge node.
//!
//! Tracks self-health, node-health, workload-health, and control-health.

use serde::{Deserialize, Serialize};

/// Aggregate health status for this node.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Health check result for the node.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeHealth {
    pub status: HealthStatus,
    pub uptime_secs: u64,
    pub vm_count: u32,
}

impl NodeHealth {
    pub fn healthy(uptime_secs: u64, vm_count: u32) -> Self {
        Self {
            status: HealthStatus::Healthy,
            uptime_secs,
            vm_count,
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
}
