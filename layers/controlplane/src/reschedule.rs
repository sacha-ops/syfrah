//! Reschedule VMs when a hypervisor goes Down.
//!
//! When gossip marks a hypervisor as Down (unreachable > threshold):
//! 1. Leader checks which VMs are on that hypervisor.
//! 2. For each VM with `restart_on_failure: true`:
//!    - Check storage safety (no local persistent volumes).
//!    - Run scheduler to find a new hypervisor.
//!    - Commit `RescheduleVm` to Raft with new placement_generation.
//!    - Target Forge creates the new VM.
//!    - Old placement is fenced (generation stale).
//! 3. VMs with local storage or GPU: NOT auto-rescheduled, marked Failed.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::gossip::GossipCluster;
use crate::scheduler::{PlacementConstraints, Scheduler, SchedulerError};

// ---------------------------------------------------------------------------
// VM Placement record (from the placement store)
// ---------------------------------------------------------------------------

/// Lightweight VM placement info for rescheduling decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmPlacementInfo {
    pub vm_id: String,
    pub hypervisor_id: String,
    pub subnet_id: String,
    pub ip: String,
    pub mac: String,
    pub generation: u64,
    /// Whether this VM should be auto-rescheduled on failure.
    pub restart_on_failure: bool,
    /// Whether this VM has local persistent storage (not reschedulable).
    pub has_local_storage: bool,
    /// Whether this VM has GPU passthrough (not reschedulable).
    pub has_gpu: bool,
}

// ---------------------------------------------------------------------------
// Reschedule decision
// ---------------------------------------------------------------------------

/// The outcome of evaluating a single VM for rescheduling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RescheduleOutcome {
    /// VM will be rescheduled to a new hypervisor.
    Rescheduled {
        vm_id: String,
        from: String,
        to: String,
        new_generation: u64,
    },
    /// VM cannot be rescheduled — marked as Failed.
    MarkedFailed { vm_id: String, reason: String },
    /// VM opted out of auto-rescheduling.
    Skipped { vm_id: String, reason: String },
}

/// Summary of a reschedule operation for a failed hypervisor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RescheduleSummary {
    pub hypervisor_id: String,
    pub rescheduled: Vec<RescheduleOutcome>,
    pub failed: Vec<RescheduleOutcome>,
    pub skipped: Vec<RescheduleOutcome>,
}

// ---------------------------------------------------------------------------
// Rescheduler
// ---------------------------------------------------------------------------

/// Rescheduler handles the process of finding new homes for VMs
/// when their hypervisor goes down.
pub struct Rescheduler<'a> {
    scheduler: &'a Scheduler,
    cluster: &'a GossipCluster,
}

impl<'a> Rescheduler<'a> {
    /// Create a new rescheduler.
    pub fn new(scheduler: &'a Scheduler, cluster: &'a GossipCluster) -> Self {
        Self { scheduler, cluster }
    }

    /// Evaluate and reschedule VMs from a failed hypervisor.
    ///
    /// Returns a summary of what happened to each VM.
    pub fn reschedule_from_failed(
        &self,
        failed_hypervisor: &str,
        vms: &[VmPlacementInfo],
    ) -> RescheduleSummary {
        let mut summary = RescheduleSummary {
            hypervisor_id: failed_hypervisor.to_string(),
            ..Default::default()
        };

        for vm in vms {
            if vm.hypervisor_id != failed_hypervisor {
                continue;
            }

            let outcome = self.evaluate_vm(vm, failed_hypervisor);
            match &outcome {
                RescheduleOutcome::Rescheduled { .. } => {
                    info!(
                        "reschedule: VM '{}' will be moved from '{}'",
                        vm.vm_id, failed_hypervisor
                    );
                    summary.rescheduled.push(outcome);
                }
                RescheduleOutcome::MarkedFailed { vm_id, reason } => {
                    warn!(
                        "reschedule: VM '{}' cannot be rescheduled: {}",
                        vm_id, reason
                    );
                    summary.failed.push(outcome);
                }
                RescheduleOutcome::Skipped { vm_id, reason } => {
                    info!("reschedule: VM '{}' skipped: {}", vm_id, reason);
                    summary.skipped.push(outcome);
                }
            }
        }

        info!(
            "reschedule: hypervisor '{}' — {} rescheduled, {} failed, {} skipped",
            failed_hypervisor,
            summary.rescheduled.len(),
            summary.failed.len(),
            summary.skipped.len()
        );

        summary
    }

    /// Evaluate a single VM for rescheduling.
    fn evaluate_vm(&self, vm: &VmPlacementInfo, failed_hypervisor: &str) -> RescheduleOutcome {
        // Check if VM opts out of auto-rescheduling.
        if !vm.restart_on_failure {
            return RescheduleOutcome::Skipped {
                vm_id: vm.vm_id.clone(),
                reason: "restart_on_failure is false".to_string(),
            };
        }

        // Check storage safety.
        if vm.has_local_storage {
            return RescheduleOutcome::MarkedFailed {
                vm_id: vm.vm_id.clone(),
                reason: "has local persistent storage — cannot auto-reschedule".to_string(),
            };
        }

        // Check GPU.
        if vm.has_gpu {
            return RescheduleOutcome::MarkedFailed {
                vm_id: vm.vm_id.clone(),
                reason: "has GPU passthrough — cannot auto-reschedule".to_string(),
            };
        }

        // Try to find a new hypervisor.
        let constraints = PlacementConstraints::default();
        let excluded = vec![failed_hypervisor.to_string()];

        match self.scheduler.schedule(
            1, // minimal resource request for rescheduling
            512,
            &constraints,
            self.cluster,
            &excluded,
            &HashMap::new(),
        ) {
            Ok(decision) => {
                let new_generation = vm.generation + 1;
                RescheduleOutcome::Rescheduled {
                    vm_id: vm.vm_id.clone(),
                    from: failed_hypervisor.to_string(),
                    to: decision.hypervisor_id,
                    new_generation,
                }
            }
            Err(SchedulerError { message, .. }) => RescheduleOutcome::MarkedFailed {
                vm_id: vm.vm_id.clone(),
                reason: format!("no available hypervisor: {message}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gossip::{GossipCluster, HypervisorGossipReport};

    fn make_report(name: &str, zone: &str) -> HypervisorGossipReport {
        HypervisorGossipReport {
            hypervisor_id: format!("{name}-id"),
            node_name: name.to_string(),
            region: "eu-west".to_string(),
            zone: zone.to_string(),
            state: "Available".to_string(),
            allocatable_vcpus: 8,
            allocatable_memory_mb: 16384,
            used_vcpus: 0,
            used_memory_mb: 0,
            instance_count: 0,
            drain_status: false,
            timestamp: 1000,
        }
    }

    fn make_vm(
        id: &str,
        hv: &str,
        restart: bool,
        local_storage: bool,
        gpu: bool,
    ) -> VmPlacementInfo {
        VmPlacementInfo {
            vm_id: id.to_string(),
            hypervisor_id: hv.to_string(),
            subnet_id: "sub-1".to_string(),
            ip: "10.0.0.5".to_string(),
            mac: "02:00:0a:00:00:05".to_string(),
            generation: 1,
            restart_on_failure: restart,
            has_local_storage: local_storage,
            has_gpu: gpu,
        }
    }

    #[test]
    fn reschedule_normal_vm() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1"));
        cluster.update_report(make_report("hv-2", "az-2"));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let rescheduler = Rescheduler::new(&scheduler, &cluster);

        let vms = vec![make_vm("vm-1", "hv-1", true, false, false)];
        let summary = rescheduler.reschedule_from_failed("hv-1", &vms);

        assert_eq!(summary.rescheduled.len(), 1);
        assert_eq!(summary.failed.len(), 0);
        assert_eq!(summary.skipped.len(), 0);

        match &summary.rescheduled[0] {
            RescheduleOutcome::Rescheduled {
                vm_id,
                from,
                to,
                new_generation,
            } => {
                assert_eq!(vm_id, "vm-1");
                assert_eq!(from, "hv-1");
                assert_eq!(to, "hv-2"); // only hv-2 available (hv-1 excluded)
                assert_eq!(*new_generation, 2);
            }
            _ => panic!("expected Rescheduled"),
        }
    }

    #[test]
    fn skip_vm_without_restart_on_failure() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1"));
        cluster.update_report(make_report("hv-2", "az-2"));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let rescheduler = Rescheduler::new(&scheduler, &cluster);

        let vms = vec![make_vm("vm-1", "hv-1", false, false, false)];
        let summary = rescheduler.reschedule_from_failed("hv-1", &vms);

        assert_eq!(summary.rescheduled.len(), 0);
        assert_eq!(summary.skipped.len(), 1);
    }

    #[test]
    fn fail_vm_with_local_storage() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1"));
        cluster.update_report(make_report("hv-2", "az-2"));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let rescheduler = Rescheduler::new(&scheduler, &cluster);

        let vms = vec![make_vm("vm-1", "hv-1", true, true, false)];
        let summary = rescheduler.reschedule_from_failed("hv-1", &vms);

        assert_eq!(summary.rescheduled.len(), 0);
        assert_eq!(summary.failed.len(), 1);
        match &summary.failed[0] {
            RescheduleOutcome::MarkedFailed { reason, .. } => {
                assert!(reason.contains("local persistent storage"));
            }
            _ => panic!("expected MarkedFailed"),
        }
    }

    #[test]
    fn fail_vm_with_gpu() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1"));
        cluster.update_report(make_report("hv-2", "az-2"));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let rescheduler = Rescheduler::new(&scheduler, &cluster);

        let vms = vec![make_vm("vm-1", "hv-1", true, false, true)];
        let summary = rescheduler.reschedule_from_failed("hv-1", &vms);

        assert_eq!(summary.rescheduled.len(), 0);
        assert_eq!(summary.failed.len(), 1);
        match &summary.failed[0] {
            RescheduleOutcome::MarkedFailed { reason, .. } => {
                assert!(reason.contains("GPU passthrough"));
            }
            _ => panic!("expected MarkedFailed"),
        }
    }

    #[test]
    fn no_available_hypervisor_for_reschedule() {
        let cluster = GossipCluster::new();
        // Only the failed hypervisor in gossip, no others
        cluster.update_report(make_report("hv-1", "az-1"));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let rescheduler = Rescheduler::new(&scheduler, &cluster);

        let vms = vec![make_vm("vm-1", "hv-1", true, false, false)];
        let summary = rescheduler.reschedule_from_failed("hv-1", &vms);

        // Should fail because hv-1 is excluded and no other hypervisors available
        assert_eq!(summary.rescheduled.len(), 0);
        assert_eq!(summary.failed.len(), 1);
    }

    #[test]
    fn multiple_vms_mixed_outcomes() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1"));
        cluster.update_report(make_report("hv-2", "az-2"));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let rescheduler = Rescheduler::new(&scheduler, &cluster);

        let vms = vec![
            make_vm("vm-normal", "hv-1", true, false, false),
            make_vm("vm-storage", "hv-1", true, true, false),
            make_vm("vm-gpu", "hv-1", true, false, true),
            make_vm("vm-no-restart", "hv-1", false, false, false),
            make_vm("vm-other-hv", "hv-2", true, false, false), // not on failed hv
        ];
        let summary = rescheduler.reschedule_from_failed("hv-1", &vms);

        assert_eq!(summary.rescheduled.len(), 1); // vm-normal
        assert_eq!(summary.failed.len(), 2); // vm-storage, vm-gpu
        assert_eq!(summary.skipped.len(), 1); // vm-no-restart
                                              // vm-other-hv is not on the failed hypervisor, so not processed
    }

    #[test]
    fn reschedule_outcome_serde() {
        let outcome = RescheduleOutcome::Rescheduled {
            vm_id: "vm-1".to_string(),
            from: "hv-1".to_string(),
            to: "hv-2".to_string(),
            new_generation: 2,
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let _: RescheduleOutcome = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn reschedule_summary_default() {
        let s = RescheduleSummary::default();
        assert!(s.rescheduled.is_empty());
        assert!(s.failed.is_empty());
        assert!(s.skipped.is_empty());
    }
}
