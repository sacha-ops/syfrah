//! Placement fencing — generation-based double-run protection.
//!
//! Every VM placement has a `placement_generation` (u64, monotonically increasing).
//! During each Forge reconciliation cycle, the fencing check compares:
//!
//! - Local generation (from the last time this node started the VM)
//! - Raft generation (from the authoritative placement store)
//!
//! Fencing triggers when:
//! 1. `local_generation < raft_generation` → VM was rescheduled elsewhere
//! 2. `hypervisor_id != this_node` → VM belongs to another node
//!
//! When fenced: stop the VM locally and clean up local resources.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{info, warn};

/// Tracks the local placement generation for each VM on this node.
pub struct FencingTracker {
    /// vm_id -> local placement generation
    local_generations: Mutex<HashMap<String, u64>>,
    /// This node's identifier (fabric IPv6 or node name).
    node_id: String,
}

/// Result of a fencing check for a single VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FencingVerdict {
    /// VM is correctly placed on this node; no action needed.
    Ok,
    /// VM was rescheduled: local generation is stale.
    Fenced { vm_id: String, reason: String },
}

impl FencingTracker {
    /// Create a new fencing tracker for the given node.
    pub fn new(node_id: String) -> Self {
        Self {
            local_generations: Mutex::new(HashMap::new()),
            node_id,
        }
    }

    /// Record a local placement with its generation.
    pub fn record_placement(&self, vm_id: &str, generation: u64) {
        let mut gens = self.local_generations.lock().unwrap();
        gens.insert(vm_id.to_string(), generation);
    }

    /// Remove a local placement record (VM deleted or fenced).
    pub fn remove_placement(&self, vm_id: &str) {
        let mut gens = self.local_generations.lock().unwrap();
        gens.remove(vm_id);
    }

    /// Check if a VM should be fenced based on the authoritative Raft placement.
    ///
    /// `raft_generation`: the placement_generation from the Raft state machine.
    /// `raft_hypervisor_id`: the hypervisor_id from the Raft placement record.
    ///
    /// Returns `Fenced` if the VM should be stopped on this node.
    pub fn check(
        &self,
        vm_id: &str,
        raft_generation: u64,
        raft_hypervisor_id: &str,
    ) -> FencingVerdict {
        let gens = self.local_generations.lock().unwrap();
        let local_gen = match gens.get(vm_id) {
            Some(&gen) => gen,
            None => return FencingVerdict::Ok, // Not tracked locally — nothing to fence.
        };

        // Case 1: VM was rescheduled elsewhere (generation advanced).
        if local_gen < raft_generation && raft_hypervisor_id != self.node_id {
            warn!(
                vm_id,
                local_gen,
                raft_generation,
                raft_node = raft_hypervisor_id,
                "fencing: VM rescheduled to another node"
            );
            return FencingVerdict::Fenced {
                vm_id: vm_id.to_string(),
                reason: format!(
                    "rescheduled: local gen {local_gen} < raft gen {raft_generation}, \
                     now on {raft_hypervisor_id}"
                ),
            };
        }

        // Case 2: Same generation but different hypervisor (stale placement).
        if local_gen == raft_generation && raft_hypervisor_id != self.node_id {
            warn!(
                vm_id,
                raft_node = raft_hypervisor_id,
                local_node = self.node_id.as_str(),
                "fencing: VM placed on different node with same generation"
            );
            return FencingVerdict::Fenced {
                vm_id: vm_id.to_string(),
                reason: format!(
                    "wrong node: placed on {raft_hypervisor_id}, not {}",
                    self.node_id
                ),
            };
        }

        FencingVerdict::Ok
    }

    /// Run fencing checks for all locally tracked VMs against Raft placements.
    ///
    /// Returns a list of VMs that should be fenced.
    pub fn check_all(
        &self,
        raft_placements: &[(String, u64, String)], // (vm_id, generation, hypervisor_id)
    ) -> Vec<FencingVerdict> {
        let mut fenced = Vec::new();
        let gens = self.local_generations.lock().unwrap();

        for (vm_id, raft_gen, raft_hv) in raft_placements {
            if !gens.contains_key(vm_id.as_str()) {
                continue; // Not a local VM.
            }
            let local_gen = gens.get(vm_id.as_str()).copied().unwrap_or(0);

            if local_gen < *raft_gen && raft_hv != &self.node_id {
                fenced.push(FencingVerdict::Fenced {
                    vm_id: vm_id.clone(),
                    reason: format!("rescheduled: local gen {local_gen} < raft gen {raft_gen}"),
                });
            } else if local_gen == *raft_gen && raft_hv != &self.node_id {
                fenced.push(FencingVerdict::Fenced {
                    vm_id: vm_id.clone(),
                    reason: format!("wrong node: placed on {raft_hv}"),
                });
            }
        }

        if !fenced.is_empty() {
            info!(
                count = fenced.len(),
                "fencing check: {} VM(s) to be fenced",
                fenced.len()
            );
        }

        fenced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_when_same_node_and_generation() {
        let tracker = FencingTracker::new("node-1".to_string());
        tracker.record_placement("vm-1", 1);
        let verdict = tracker.check("vm-1", 1, "node-1");
        assert_eq!(verdict, FencingVerdict::Ok);
    }

    #[test]
    fn fenced_when_rescheduled() {
        let tracker = FencingTracker::new("node-1".to_string());
        tracker.record_placement("vm-1", 1);
        let verdict = tracker.check("vm-1", 2, "node-2");
        assert!(matches!(verdict, FencingVerdict::Fenced { .. }));
    }

    #[test]
    fn fenced_when_wrong_node_same_generation() {
        let tracker = FencingTracker::new("node-1".to_string());
        tracker.record_placement("vm-1", 1);
        let verdict = tracker.check("vm-1", 1, "node-2");
        assert!(matches!(verdict, FencingVerdict::Fenced { .. }));
    }

    #[test]
    fn ok_when_no_local_placement() {
        let tracker = FencingTracker::new("node-1".to_string());
        // No local placement recorded.
        let verdict = tracker.check("vm-99", 1, "node-2");
        assert_eq!(verdict, FencingVerdict::Ok);
    }

    #[test]
    fn check_all_returns_fenced_vms() {
        let tracker = FencingTracker::new("node-1".to_string());
        tracker.record_placement("vm-1", 1);
        tracker.record_placement("vm-2", 1);

        let raft_placements = vec![
            ("vm-1".to_string(), 1, "node-1".to_string()), // ok
            ("vm-2".to_string(), 2, "node-2".to_string()), // fenced
        ];

        let fenced = tracker.check_all(&raft_placements);
        assert_eq!(fenced.len(), 1);
        assert!(matches!(&fenced[0], FencingVerdict::Fenced { vm_id, .. } if vm_id == "vm-2"));
    }

    #[test]
    fn remove_stops_tracking() {
        let tracker = FencingTracker::new("node-1".to_string());
        tracker.record_placement("vm-1", 1);
        tracker.remove_placement("vm-1");
        // After removal, check should return Ok (not tracking this VM).
        let verdict = tracker.check("vm-1", 2, "node-2");
        assert_eq!(verdict, FencingVerdict::Ok);
    }
}
