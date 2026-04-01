//! Compensating cleanup on failure.
//!
//! When a create/update operation fails mid-way, the `RollbackTracker`
//! records what was completed and reverses those steps in reverse order.
//!
//! This module covers the full Forge lifecycle (not just network):
//! - IPAM allocation
//! - Bridge creation
//! - VXLAN creation
//! - TAP creation
//! - Bridge attachment
//! - nftables rules
//! - NAT masquerade
//! - FDB entries
//! - ARP proxy entries
//! - Capacity reservation
//!
//! Rollback is best-effort: each step is attempted independently.
//! Residuals are caught by the reconciliation loop.

use serde::{Deserialize, Serialize};
use tracing::warn;

/// A single completed step that may need rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompletedStep {
    /// IP was allocated from IPAM.
    IpAllocated { subnet_id: String, ip: String },
    /// Bridge was created for a VPC.
    BridgeCreated { bridge_name: String },
    /// VXLAN was created for a VPC.
    VxlanCreated { vxlan_name: String },
    /// TAP device was created.
    TapCreated { tap_name: String },
    /// TAP was attached to a bridge.
    BridgeAttached {
        tap_name: String,
        bridge_name: String,
    },
    /// nftables rules were applied for a VM.
    NftablesApplied { tap_name: String },
    /// NAT masquerade was applied.
    NatApplied {
        bridge_name: String,
        subnet_cidr: String,
    },
    /// FDB entry was added.
    FdbAdded { bridge_name: String, mac: String },
    /// ARP proxy entry was added.
    ArpProxyAdded { vxlan_name: String, ip: String },
    /// Capacity was reserved.
    CapacityReserved {
        name: String,
        vcpus: u32,
        memory_mb: u64,
    },
}

/// Callback for releasing an IP allocation.
pub type IpReleaseCallback = Box<dyn FnOnce(&str, &str) + Send>;

/// Callback for releasing capacity.
pub type CapacityReleaseCallback = Box<dyn FnOnce(&str, u32, u64) + Send>;

/// Tracks completed steps during a multi-step operation for rollback.
pub struct RollbackTracker {
    steps: Vec<CompletedStep>,
}

/// Result of a rollback: how many steps were attempted and how many failed.
#[derive(Debug, Default)]
pub struct RollbackResult {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

impl RollbackTracker {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Record a completed step.
    pub fn record(&mut self, step: CompletedStep) {
        self.steps.push(step);
    }

    /// Get a reference to the recorded steps.
    pub fn steps(&self) -> &[CompletedStep] {
        &self.steps
    }

    /// How many steps have been recorded.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether any steps have been recorded.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Roll back all recorded steps in reverse order.
    ///
    /// Best-effort: each step is attempted independently. Failures are
    /// logged but do not prevent subsequent rollback steps.
    ///
    /// `ip_release` and `capacity_release` are optional callbacks for
    /// releasing IPAM allocations and capacity reservations, since the
    /// forge/cleanup module does not own those resources directly.
    pub async fn rollback(
        self,
        backend: &dyn syfrah_overlay::NetworkBackend,
        ip_release: Option<IpReleaseCallback>,
        capacity_release: Option<CapacityReleaseCallback>,
    ) -> RollbackResult {
        let mut result = RollbackResult::default();

        // Reverse order.
        for step in self.steps.into_iter().rev() {
            result.attempted += 1;
            match step {
                CompletedStep::CapacityReserved {
                    name,
                    vcpus,
                    memory_mb,
                } => {
                    if capacity_release.is_some() {
                        // Capacity release callback can't be called multiple times (FnOnce).
                        // Log and let the reconciler handle residual capacity.
                        warn!(
                            name = %name,
                            vcpus,
                            memory_mb,
                            "rollback: capacity reserved (caller must release)"
                        );
                    }
                    result.succeeded += 1;
                }
                CompletedStep::ArpProxyAdded { vxlan_name, ip } => {
                    if let Err(e) = backend.remove_arp_proxy(&vxlan_name, &ip).await {
                        warn!(vxlan = %vxlan_name, ip = %ip, error = %e, "rollback: failed to remove ARP proxy");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::FdbAdded { bridge_name, mac } => {
                    if let Err(e) = backend.remove_fdb_entry(&bridge_name, &mac).await {
                        warn!(bridge = %bridge_name, mac = %mac, error = %e, "rollback: failed to remove FDB entry");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::NatApplied {
                    bridge_name,
                    subnet_cidr,
                } => {
                    if let Err(e) = backend.remove_nat(&bridge_name, &subnet_cidr).await {
                        warn!(bridge = %bridge_name, subnet = %subnet_cidr, error = %e, "rollback: failed to remove NAT");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::NftablesApplied { tap_name } => {
                    if let Err(e) = backend.remove_vm_rules(&tap_name).await {
                        warn!(tap = %tap_name, error = %e, "rollback: failed to remove nftables rules");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::BridgeAttached { .. } => {
                    // Detaching from bridge is implicit when TAP is deleted.
                    result.succeeded += 1;
                }
                CompletedStep::TapCreated { tap_name } => {
                    if let Err(e) = backend.delete_tap(&tap_name).await {
                        warn!(tap = %tap_name, error = %e, "rollback: failed to delete TAP");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::VxlanCreated { vxlan_name } => {
                    if let Err(e) = backend.delete_vxlan(&vxlan_name).await {
                        warn!(vxlan = %vxlan_name, error = %e, "rollback: failed to delete VXLAN");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::BridgeCreated { bridge_name } => {
                    if let Err(e) = backend.delete_bridge(&bridge_name).await {
                        warn!(bridge = %bridge_name, error = %e, "rollback: failed to delete bridge");
                        result.failed += 1;
                    } else {
                        result.succeeded += 1;
                    }
                }
                CompletedStep::IpAllocated { subnet_id, ip } => {
                    if let Some(release_fn) = ip_release {
                        release_fn(&subnet_id, &ip);
                        result.succeeded += 1;
                    } else {
                        warn!(
                            subnet = %subnet_id,
                            ip = %ip,
                            "rollback: IP allocated but no release callback provided"
                        );
                        result.failed += 1;
                    }
                    // ip_release is consumed; subsequent IpAllocated steps
                    // won't have a callback. Normally there's only one per
                    // tracker.
                    break;
                }
            }
        }

        result
    }
}

impl Default for RollbackTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syfrah_overlay::MockBackend;

    #[test]
    fn empty_tracker() {
        let tracker = RollbackTracker::new();
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);
    }

    #[test]
    fn record_steps() {
        let mut tracker = RollbackTracker::new();
        tracker.record(CompletedStep::BridgeCreated {
            bridge_name: "syfb-123".to_string(),
        });
        tracker.record(CompletedStep::TapCreated {
            tap_name: "syft-456".to_string(),
        });
        assert_eq!(tracker.len(), 2);
        assert!(!tracker.is_empty());
    }

    #[tokio::test]
    async fn rollback_reverse_order() {
        let backend = MockBackend::new();
        let mut tracker = RollbackTracker::new();

        tracker.record(CompletedStep::BridgeCreated {
            bridge_name: "syfb-123".to_string(),
        });
        tracker.record(CompletedStep::TapCreated {
            tap_name: "syft-456".to_string(),
        });
        tracker.record(CompletedStep::NftablesApplied {
            tap_name: "syft-456".to_string(),
        });

        let result = tracker.rollback(&backend, None, None).await;
        assert_eq!(result.attempted, 3);
        assert_eq!(result.succeeded, 3);
        assert_eq!(result.failed, 0);

        // Verify reverse order: nft rules removed, then TAP, then bridge.
        let calls = backend.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].starts_with("remove_vm_rules("));
        assert!(calls[1].starts_with("delete_tap("));
        assert!(calls[2].starts_with("delete_bridge("));
    }

    #[tokio::test]
    async fn rollback_best_effort_on_failure() {
        let backend = MockBackend::new();
        backend.set_fail("delete_tap");

        let mut tracker = RollbackTracker::new();
        tracker.record(CompletedStep::BridgeCreated {
            bridge_name: "syfb-123".to_string(),
        });
        tracker.record(CompletedStep::TapCreated {
            tap_name: "syft-456".to_string(),
        });

        let result = tracker.rollback(&backend, None, None).await;
        assert_eq!(result.attempted, 2);
        // TAP delete fails, but bridge delete still runs.
        assert!(result.failed >= 1);
    }

    #[tokio::test]
    async fn rollback_with_ip_release() {
        let backend = MockBackend::new();
        let released = std::sync::Arc::new(std::sync::Mutex::new(false));
        let released_clone = std::sync::Arc::clone(&released);

        let mut tracker = RollbackTracker::new();
        tracker.record(CompletedStep::IpAllocated {
            subnet_id: "subnet-1".to_string(),
            ip: "10.1.0.3".to_string(),
        });

        let release_cb: IpReleaseCallback = Box::new(move |_s, _i| {
            *released_clone.lock().unwrap() = true;
        });

        let result = tracker.rollback(&backend, Some(release_cb), None).await;
        assert_eq!(result.succeeded, 1);
        assert!(*released.lock().unwrap());
    }

    #[tokio::test]
    async fn rollback_full_lifecycle() {
        let backend = MockBackend::new();
        let mut tracker = RollbackTracker::new();

        // Simulate a full VM creation sequence.
        tracker.record(CompletedStep::BridgeCreated {
            bridge_name: "syfb-123".to_string(),
        });
        tracker.record(CompletedStep::VxlanCreated {
            vxlan_name: "syfx-123".to_string(),
        });
        tracker.record(CompletedStep::TapCreated {
            tap_name: "syft-456".to_string(),
        });
        tracker.record(CompletedStep::BridgeAttached {
            tap_name: "syft-456".to_string(),
            bridge_name: "syfb-123".to_string(),
        });
        tracker.record(CompletedStep::NftablesApplied {
            tap_name: "syft-456".to_string(),
        });
        tracker.record(CompletedStep::NatApplied {
            bridge_name: "syfb-123".to_string(),
            subnet_cidr: "10.1.0.0/24".to_string(),
        });
        tracker.record(CompletedStep::FdbAdded {
            bridge_name: "syfb-123".to_string(),
            mac: "02:00:0a:01:00:03".to_string(),
        });

        let result = tracker.rollback(&backend, None, None).await;
        assert_eq!(result.attempted, 7);
        assert_eq!(result.succeeded, 7);

        // Verify cleanup happened in reverse order.
        let calls = backend.calls();
        assert!(calls[0].starts_with("remove_fdb_entry("));
        assert!(calls[1].starts_with("remove_nat("));
        assert!(calls[2].starts_with("remove_vm_rules("));
        // BridgeAttached is a no-op (implicit with TAP delete).
        assert!(calls[3].starts_with("delete_tap("));
        assert!(calls[4].starts_with("delete_vxlan("));
        assert!(calls[5].starts_with("delete_bridge("));
    }

    #[test]
    fn completed_step_serializes() {
        let step = CompletedStep::BridgeCreated {
            bridge_name: "syfb-123".to_string(),
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("syfb-123"));
    }
}
