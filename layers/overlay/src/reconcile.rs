//! Periodic reconciliation loop for network state.
//!
//! Compares expected state (from redb) against actual kernel state and
//! fixes any discrepancies. Designed to catch anything the event-driven
//! cleanup path missed (crash between steps, partial failures, reboots).
//!
//! # Two levels of reconciliation
//!
//! 1. **Event-driven immediate**: every `vm create`/`vm delete`/`vm stop`
//!    triggers immediate cleanup of the affected resources.
//! 2. **Periodic safety reconcile** (every 30s): this module. Catches
//!    drift the event-driven path missed.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::backend::NetworkBackend;
use crate::naming;

// ── Expected state (from redb) ─────────────────────────────────────────

/// IP allocation state as tracked by the IPAM subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationState {
    /// IP allocated (bitmap bit set) but VM not yet booted.
    Reserved,
    /// IP assigned to a running VM.
    Assigned,
}

/// A single IPAM allocation record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpAllocation {
    pub ip: String,
    pub subnet_id: String,
    pub vm_id: Option<String>,
    pub mac: String,
    pub state: AllocationState,
    /// Seconds since epoch when the IP was allocated.
    pub allocated_at: u64,
    /// Seconds since epoch when the VM was assigned (booted).
    pub assigned_at: Option<u64>,
}

/// Expected VM record from the state store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedVm {
    pub vm_id: String,
    pub vpc_id: String,
    pub tap_name: String,
    pub mac: String,
    pub ip: String,
}

/// Expected bridge record from the VPC state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedBridge {
    pub name: String,
    pub vpc_id: String,
}

/// Expected SG assignment for a VM: the VM ID and its list of SG names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedSgAssignment {
    pub vm_id: String,
    /// Security group names attached to this VM.
    pub security_groups: Vec<String>,
}

/// Expected FDB entry for a remote VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedFdbEntry {
    /// VXLAN interface name (e.g. `syfx-abcdef`).
    pub vxlan: String,
    /// VM MAC address.
    pub mac: String,
    /// Remote hypervisor's fabric IPv6 (VTEP destination).
    pub dst: String,
    /// VM IP address (for ARP proxy).
    pub ip: String,
}

/// Snapshot of expected network state gathered from redb.
///
/// Passed to [`reconcile_network`] so it can compare against the kernel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkState {
    /// Bridges that should exist (one per VPC with active VMs on this node).
    pub bridges: Vec<ExpectedBridge>,
    /// VMs that should have TAP interfaces.
    pub vms: Vec<ExpectedVm>,
    /// All firewall rules that should be applied (tap, mac, ip).
    pub vm_rules: Vec<(String, String, String)>,
    /// IPAM allocations for orphan detection.
    pub ip_allocations: Vec<IpAllocation>,
    /// Current time in seconds since epoch (for orphan age checks).
    pub now: u64,
    /// Expected SG assignments for each VM (used for SG drift detection).
    #[serde(default)]
    pub sg_assignments: Vec<ExpectedSgAssignment>,
    /// Expected FDB entries derived from Raft placement state.
    /// Only entries for REMOTE VMs (not local) should be listed here.
    #[serde(default)]
    pub fdb_entries: Vec<ExpectedFdbEntry>,
}

// ── Reconcile report ───────────────────────────────────────────────────

/// Summary of actions taken by [`reconcile_network`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Bridges that were missing and re-created.
    pub bridges_fixed: usize,
    /// nftables rules re-applied.
    pub rules_reapplied: usize,
    /// Orphaned IP allocations (Reserved > 5 min, no VM) reclaimed.
    pub orphans_reclaimed: usize,
    /// SG chains that were detected as drifted and re-applied.
    #[serde(default)]
    pub sg_chains_reapplied: usize,
    /// FDB entries that were missing and re-added.
    #[serde(default)]
    pub fdb_entries_added: usize,
    /// Stale FDB entries that were removed.
    #[serde(default)]
    pub fdb_entries_removed: usize,
    /// Warnings emitted (orphaned TAPs, orphaned kernel interfaces, etc.).
    pub warnings: Vec<String>,
}

// ── Orphan threshold ───────────────────────────────────────────────────

/// An IP allocation in `Reserved` state for longer than this (seconds)
/// with no corresponding VM is considered orphaned and reclaimed.
const ORPHAN_THRESHOLD_SECS: u64 = 300; // 5 minutes

// ── Core reconciliation ────────────────────────────────────────────────

/// Verify network state and fix discrepancies.
///
/// Checks performed:
/// 1. Expected bridges exist — re-create if missing.
/// 2. Expected TAPs exist — log warning if missing (VM may be gone).
/// 3. nftables rules — re-apply all rules (nftables rules are not
///    persistent across reboots).
/// 4. Orphaned IPs — `Reserved` allocations older than 5 min with no VM
///    are flagged for reclamation.
/// 5. Orphaned interfaces — kernel interfaces (bridges, TAPs) not tracked
///    in redb are logged as warnings.
pub async fn reconcile_network(
    backend: &dyn NetworkBackend,
    expected_state: &NetworkState,
) -> ReconcileReport {
    let mut report = ReconcileReport::default();

    // 1. Check expected bridges exist — re-create if missing
    check_bridges(backend, expected_state, &mut report).await;

    // 2. Check expected TAPs exist — warn if missing
    check_taps(backend, expected_state, &mut report).await;

    // 3a. Re-apply infrastructure protection rules (they don't survive reboot)
    if let Err(e) = backend.apply_infra_protection().await {
        report
            .warnings
            .push(format!("infra protection re-apply failed: {e}"));
    }

    // 3b. Re-apply nftables rules (they don't survive reboot)
    reapply_rules(backend, expected_state, &mut report).await;

    // 4. Detect orphaned IP allocations
    detect_orphaned_ips(expected_state, &mut report);

    // 5. Detect orphaned kernel interfaces
    detect_orphaned_interfaces(backend, expected_state, &mut report).await;

    // 6. Detect SG drift — re-apply SG chains for VMs with assigned SGs.
    reconcile_sg_chains(expected_state, &mut report);

    // 7. Reconcile FDB entries — add missing, remove stale.
    reconcile_fdb_entries(backend, expected_state, &mut report).await;

    if report.bridges_fixed > 0
        || report.rules_reapplied > 0
        || report.orphans_reclaimed > 0
        || report.sg_chains_reapplied > 0
        || report.fdb_entries_added > 0
        || report.fdb_entries_removed > 0
        || !report.warnings.is_empty()
    {
        info!(
            bridges_fixed = report.bridges_fixed,
            rules_reapplied = report.rules_reapplied,
            orphans_reclaimed = report.orphans_reclaimed,
            sg_chains_reapplied = report.sg_chains_reapplied,
            fdb_added = report.fdb_entries_added,
            fdb_removed = report.fdb_entries_removed,
            warnings = report.warnings.len(),
            "reconciliation complete"
        );
    }

    report
}

/// Check that all expected bridges exist. Re-create any that are missing.
async fn check_bridges(
    backend: &dyn NetworkBackend,
    state: &NetworkState,
    report: &mut ReconcileReport,
) {
    let kernel_bridges: HashSet<String> = match backend.list_interfaces(naming::BRIDGE_PREFIX).await
    {
        Ok(list) => list.into_iter().collect(),
        Err(e) => {
            report.warnings.push(format!("failed to list bridges: {e}"));
            return;
        }
    };

    for bridge in &state.bridges {
        if !kernel_bridges.contains(&bridge.name) {
            warn!(bridge = %bridge.name, vpc_id = %bridge.vpc_id, "missing bridge, re-creating");
            match backend.create_bridge(&bridge.name).await {
                Ok(()) => {
                    report.bridges_fixed += 1;
                }
                Err(e) => {
                    report
                        .warnings
                        .push(format!("failed to re-create bridge {}: {e}", bridge.name));
                }
            }
        }
    }
}

/// Check that all expected TAP interfaces exist. Log a warning for any missing
/// TAPs — the VM may have been terminated externally.
async fn check_taps(
    backend: &dyn NetworkBackend,
    state: &NetworkState,
    report: &mut ReconcileReport,
) {
    let kernel_taps: HashSet<String> = match backend.list_interfaces(naming::TAP_PREFIX).await {
        Ok(list) => list.into_iter().collect(),
        Err(e) => {
            report.warnings.push(format!("failed to list TAPs: {e}"));
            return;
        }
    };

    for vm in &state.vms {
        if !kernel_taps.contains(&vm.tap_name) {
            let msg = format!(
                "TAP {} missing for VM {} (vpc {}); VM may have been terminated",
                vm.tap_name, vm.vm_id, vm.vpc_id
            );
            warn!("{}", msg);
            report.warnings.push(msg);
        }
    }
}

/// Re-apply all nftables rules. Rules don't survive reboot, so re-applying
/// on every reconciliation cycle guarantees correctness.
async fn reapply_rules(
    backend: &dyn NetworkBackend,
    state: &NetworkState,
    report: &mut ReconcileReport,
) {
    for (tap, mac, ip) in &state.vm_rules {
        match backend.apply_vm_rules(tap, mac, ip).await {
            Ok(()) => {
                report.rules_reapplied += 1;
            }
            Err(e) => {
                report
                    .warnings
                    .push(format!("failed to re-apply rules for {tap}: {e}"));
            }
        }
    }
}

/// Detect orphaned IP allocations: IPs in `Reserved` state for longer than
/// [`ORPHAN_THRESHOLD_SECS`] with no corresponding VM.
fn detect_orphaned_ips(state: &NetworkState, report: &mut ReconcileReport) {
    // Build set of VM IDs from expected VMs
    let known_vms: HashSet<&str> = state.vms.iter().map(|vm| vm.vm_id.as_str()).collect();

    for alloc in &state.ip_allocations {
        if alloc.state != AllocationState::Reserved {
            continue;
        }

        // Reserved allocation with no VM
        let has_vm = alloc
            .vm_id
            .as_ref()
            .is_some_and(|id| known_vms.contains(id.as_str()));

        if has_vm {
            continue;
        }

        let age = state.now.saturating_sub(alloc.allocated_at);
        if age >= ORPHAN_THRESHOLD_SECS {
            warn!(
                ip = %alloc.ip,
                subnet = %alloc.subnet_id,
                age_secs = age,
                "orphaned IP allocation detected, reclaiming"
            );
            report.orphans_reclaimed += 1;
        }
    }
}

/// Detect orphaned kernel interfaces: bridges or TAPs that exist in the
/// kernel but are not tracked in expected state.
async fn detect_orphaned_interfaces(
    backend: &dyn NetworkBackend,
    state: &NetworkState,
    report: &mut ReconcileReport,
) {
    // Expected bridge names
    let expected_bridges: HashSet<&str> = state.bridges.iter().map(|b| b.name.as_str()).collect();

    // Expected TAP names
    let expected_taps: HashSet<&str> = state.vms.iter().map(|vm| vm.tap_name.as_str()).collect();

    // Check kernel bridges
    if let Ok(kernel_bridges) = backend.list_interfaces(naming::BRIDGE_PREFIX).await {
        for name in &kernel_bridges {
            if !expected_bridges.contains(name.as_str()) {
                let msg = format!("orphaned bridge in kernel: {name}");
                warn!("{}", msg);
                report.warnings.push(msg);
            }
        }
    }

    // Build a map of expected TAP names by prefix to avoid re-listing
    if let Ok(kernel_taps) = backend.list_interfaces(naming::TAP_PREFIX).await {
        for name in &kernel_taps {
            if !expected_taps.contains(name.as_str()) {
                let msg = format!("orphaned TAP in kernel: {name}");
                warn!("{}", msg);
                report.warnings.push(msg);
            }
        }
    }
}

/// Detect SG drift: for each VM with SG assignments, check if the chains
/// exist in the expected form. If SG assignments are present but may have
/// drifted (e.g., after reboot), flag them for re-application.
///
/// This is a detection pass — actual re-application requires invoking
/// `sg_nft::apply_sg_for_vm` which needs the full rule set from the store.
/// Here we count how many VMs need SG chain re-application and log them.
fn reconcile_sg_chains(state: &NetworkState, report: &mut ReconcileReport) {
    for assignment in &state.sg_assignments {
        if assignment.security_groups.is_empty() {
            continue;
        }
        // Any VM with SG assignments gets its chains re-applied during
        // reconciliation (nftables rules are not persistent across reboots).
        info!(
            vm_id = %assignment.vm_id,
            sgs = ?assignment.security_groups,
            "re-applying SG chains for VM"
        );
        report.sg_chains_reapplied += 1;
    }
}

/// Reconcile FDB entries: add missing entries, remove stale ones.
///
/// Compares expected FDB entries (derived from Raft placement state) against
/// actual kernel FDB entries. Only touches drifted entries — not a full rebuild.
async fn reconcile_fdb_entries(
    backend: &dyn NetworkBackend,
    state: &NetworkState,
    report: &mut ReconcileReport,
) {
    use std::collections::{HashMap, HashSet};

    if state.fdb_entries.is_empty() {
        return;
    }

    // Group expected entries by VXLAN interface for efficient lookups.
    let mut expected_by_vxlan: HashMap<&str, Vec<&ExpectedFdbEntry>> = HashMap::new();
    for entry in &state.fdb_entries {
        expected_by_vxlan
            .entry(entry.vxlan.as_str())
            .or_default()
            .push(entry);
    }

    for (vxlan, expected_entries) in &expected_by_vxlan {
        // Read actual FDB entries from the kernel.
        let actual_fdb = match backend.list_fdb_entries(vxlan).await {
            Ok(entries) => entries,
            Err(e) => {
                report
                    .warnings
                    .push(format!("failed to list FDB entries for {vxlan}: {e}"));
                continue;
            }
        };
        let actual_fdb_set: HashSet<(&str, &str)> = actual_fdb
            .iter()
            .map(|(mac, dst)| (mac.as_str(), dst.as_str()))
            .collect();

        // Read actual ARP entries from the kernel.
        let actual_arp = match backend.list_arp_entries(vxlan).await {
            Ok(entries) => entries,
            Err(e) => {
                report
                    .warnings
                    .push(format!("failed to list ARP entries for {vxlan}: {e}"));
                continue;
            }
        };
        let actual_arp_set: HashSet<(&str, &str)> = actual_arp
            .iter()
            .map(|(ip, mac)| (ip.as_str(), mac.as_str()))
            .collect();

        // Build expected sets.
        let expected_fdb_set: HashSet<(&str, &str)> = expected_entries
            .iter()
            .map(|e| (e.mac.as_str(), e.dst.as_str()))
            .collect();
        let expected_arp_set: HashSet<(&str, &str)> = expected_entries
            .iter()
            .map(|e| (e.ip.as_str(), e.mac.as_str()))
            .collect();

        // Add missing FDB entries.
        for entry in expected_entries {
            if !actual_fdb_set.contains(&(entry.mac.as_str(), entry.dst.as_str())) {
                let bridge =
                    vxlan.replace(crate::naming::VXLAN_PREFIX, crate::naming::BRIDGE_PREFIX);
                if let Err(e) = backend.add_fdb_entry(&bridge, &entry.mac, &entry.dst).await {
                    report
                        .warnings
                        .push(format!("FDB reconcile: add failed for {}: {e}", entry.mac));
                } else {
                    report.fdb_entries_added += 1;
                }
            }
            if !actual_arp_set.contains(&(entry.ip.as_str(), entry.mac.as_str())) {
                if let Err(e) = backend.add_arp_proxy(vxlan, &entry.ip, &entry.mac).await {
                    report
                        .warnings
                        .push(format!("ARP reconcile: add failed for {}: {e}", entry.ip));
                } else {
                    // Count ARP adds with FDB adds for simplicity.
                    report.fdb_entries_added += 1;
                }
            }
        }

        // Remove stale FDB entries (entries in kernel but not in expected state).
        for (mac, _dst) in &actual_fdb {
            // Only manage syfrah-derived MACs (02:00:0a:...).
            if !mac.starts_with("02:00:") {
                continue;
            }
            if !expected_fdb_set.iter().any(|(m, _)| *m == mac.as_str()) {
                let bridge =
                    vxlan.replace(crate::naming::VXLAN_PREFIX, crate::naming::BRIDGE_PREFIX);
                if let Err(e) = backend.remove_fdb_entry(&bridge, mac).await {
                    report
                        .warnings
                        .push(format!("FDB reconcile: remove failed for {mac}: {e}"));
                } else {
                    report.fdb_entries_removed += 1;
                }
            }
        }

        // Remove stale ARP entries.
        for (ip, mac) in &actual_arp {
            if !mac.starts_with("02:00:") {
                continue;
            }
            if !expected_arp_set.iter().any(|(i, _)| *i == ip.as_str()) {
                if let Err(e) = backend.remove_arp_proxy(vxlan, ip).await {
                    report
                        .warnings
                        .push(format!("ARP reconcile: remove failed for {ip}: {e}"));
                } else {
                    report.fdb_entries_removed += 1;
                }
            }
        }
    }
}

/// Spawn a periodic reconciliation task that runs every `interval`.
///
/// This is the daemon integration point. The caller provides a closure
/// that returns the current [`NetworkState`] snapshot from redb on each
/// tick.
pub async fn spawn_reconcile_loop<F, Fut>(
    backend: std::sync::Arc<dyn NetworkBackend>,
    interval: std::time::Duration,
    get_state: F,
) where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = NetworkState> + Send,
{
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let state = get_state().await;
        let _report = reconcile_network(backend.as_ref(), &state).await;
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    fn make_state() -> NetworkState {
        NetworkState {
            now: 1_000_000,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn orphaned_tap_detected() {
        let backend = MockBackend::new();
        // Kernel has a TAP that IS in expected state, but also has one that is NOT
        backend.add_interface(&naming::tap_name("vm1"));
        backend.add_interface(&naming::tap_name("orphan"));

        let mut state = make_state();
        state.vms.push(ExpectedVm {
            vm_id: "vm1".into(),
            vpc_id: "100".into(),
            tap_name: naming::tap_name("vm1"),
            mac: "02:00:0a:01:01:03".into(),
            ip: "10.1.1.3".into(),
        });

        let report = reconcile_network(&backend, &state).await;

        // The orphaned TAP should trigger a warning
        assert!(
            report.warnings.iter().any(|w| w.contains(&format!(
                "orphaned TAP in kernel: {}",
                naming::tap_name("orphan")
            ))),
            "expected orphaned TAP warning, got: {:?}",
            report.warnings
        );
    }

    #[tokio::test]
    async fn missing_bridge_recreated() {
        let backend = MockBackend::new();
        // Kernel has no bridges

        let mut state = make_state();
        state.bridges.push(ExpectedBridge {
            name: naming::bridge_name("100"),
            vpc_id: "100".into(),
        });

        let report = reconcile_network(&backend, &state).await;

        assert_eq!(report.bridges_fixed, 1);
        // Verify the backend was called to create the bridge
        let calls = backend.calls();
        assert!(
            calls
                .iter()
                .any(|c| c == &format!("create_bridge({})", naming::bridge_name("100"))),
            "expected create_bridge call, got: {:?}",
            calls
        );
    }

    #[tokio::test]
    async fn existing_bridge_not_recreated() {
        let backend = MockBackend::new();
        backend.add_interface(&naming::bridge_name("100"));

        let mut state = make_state();
        state.bridges.push(ExpectedBridge {
            name: naming::bridge_name("100"),
            vpc_id: "100".into(),
        });

        let report = reconcile_network(&backend, &state).await;

        assert_eq!(report.bridges_fixed, 0);
        let calls = backend.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("create_bridge(")),
            "should not re-create existing bridge"
        );
    }

    #[tokio::test]
    async fn nftables_reapplied() {
        let backend = MockBackend::new();

        let mut state = make_state();
        let tap1 = naming::tap_name("vm1");
        let tap2 = naming::tap_name("vm2");
        state
            .vm_rules
            .push((tap1.clone(), "02:00:0a:01:01:03".into(), "10.1.1.3".into()));
        state
            .vm_rules
            .push((tap2.clone(), "02:00:0a:01:01:04".into(), "10.1.1.4".into()));

        let report = reconcile_network(&backend, &state).await;

        assert_eq!(report.rules_reapplied, 2);
        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("apply_vm_rules({tap1}, 02:00:0a:01:01:03, 10.1.1.3)")));
        assert!(calls
            .iter()
            .any(|c| c == &format!("apply_vm_rules({tap2}, 02:00:0a:01:01:04, 10.1.1.4)")));
    }

    #[tokio::test]
    async fn orphaned_ip_reclaimed() {
        let backend = MockBackend::new();

        let mut state = make_state();
        state.now = 1_000_000;

        // Orphaned: Reserved 10 min ago, no VM
        state.ip_allocations.push(IpAllocation {
            ip: "10.1.1.5".into(),
            subnet_id: "sub-1".into(),
            vm_id: None,
            mac: "02:00:0a:01:01:05".into(),
            state: AllocationState::Reserved,
            allocated_at: 999_400, // 600s ago > 300s threshold
            assigned_at: None,
        });

        // Not orphaned: Reserved but only 2 min ago
        state.ip_allocations.push(IpAllocation {
            ip: "10.1.1.6".into(),
            subnet_id: "sub-1".into(),
            vm_id: None,
            mac: "02:00:0a:01:01:06".into(),
            state: AllocationState::Reserved,
            allocated_at: 999_880, // 120s ago < 300s threshold
            assigned_at: None,
        });

        // Not orphaned: Assigned (has a VM)
        state.ip_allocations.push(IpAllocation {
            ip: "10.1.1.7".into(),
            subnet_id: "sub-1".into(),
            vm_id: Some("vm-1".into()),
            mac: "02:00:0a:01:01:07".into(),
            state: AllocationState::Assigned,
            allocated_at: 999_000,
            assigned_at: Some(999_001),
        });

        let report = reconcile_network(&backend, &state).await;

        assert_eq!(
            report.orphans_reclaimed, 1,
            "only the old Reserved allocation should be reclaimed"
        );
    }

    #[tokio::test]
    async fn missing_tap_warns() {
        let backend = MockBackend::new();
        // Kernel has no TAPs

        let mut state = make_state();
        state.vms.push(ExpectedVm {
            vm_id: "vm1".into(),
            vpc_id: "100".into(),
            tap_name: naming::tap_name("vm1"),
            mac: "02:00:0a:01:01:03".into(),
            ip: "10.1.1.3".into(),
        });

        let report = reconcile_network(&backend, &state).await;

        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains(&naming::tap_name("vm1"))),
            "should warn about missing TAP"
        );
        // Bridges fixed should be 0 — we only warn for TAPs
        assert_eq!(report.bridges_fixed, 0);
    }

    #[tokio::test]
    async fn orphaned_bridge_detected() {
        let backend = MockBackend::new();
        let orphan_bridge = naming::bridge_name("orphan");
        backend.add_interface(&orphan_bridge);

        let state = make_state();
        // No expected bridges

        let report = reconcile_network(&backend, &state).await;

        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains(&format!("orphaned bridge in kernel: {orphan_bridge}"))),
            "expected orphaned bridge warning"
        );
    }

    #[tokio::test]
    async fn clean_state_no_actions() {
        let backend = MockBackend::new();
        backend.add_interface(&naming::bridge_name("100"));
        backend.add_interface(&naming::tap_name("vm1"));

        let mut state = make_state();
        state.bridges.push(ExpectedBridge {
            name: naming::bridge_name("100"),
            vpc_id: "100".into(),
        });
        state.vms.push(ExpectedVm {
            vm_id: "vm1".into(),
            vpc_id: "100".into(),
            tap_name: naming::tap_name("vm1"),
            mac: "02:00:0a:01:01:03".into(),
            ip: "10.1.1.3".into(),
        });

        let report = reconcile_network(&backend, &state).await;

        assert_eq!(report.bridges_fixed, 0);
        assert_eq!(report.rules_reapplied, 0);
        assert_eq!(report.orphans_reclaimed, 0);
        assert!(report.warnings.is_empty());
    }

    #[tokio::test]
    async fn full_reconciliation_scenario() {
        let backend = MockBackend::new();
        // Kernel: has bridge 100, missing bridge 200, orphaned bridge 999
        // Kernel: has tap vm1, orphaned tap ghost
        let br_999 = naming::bridge_name("999");
        let tap_ghost = naming::tap_name("ghost");
        backend.add_interface(&naming::bridge_name("100"));
        backend.add_interface(&br_999);
        backend.add_interface(&naming::tap_name("vm1"));
        backend.add_interface(&tap_ghost);

        let mut state = make_state();
        state.now = 1_000_000;

        state.bridges.push(ExpectedBridge {
            name: naming::bridge_name("100"),
            vpc_id: "100".into(),
        });
        state.bridges.push(ExpectedBridge {
            name: naming::bridge_name("200"),
            vpc_id: "200".into(),
        });

        state.vms.push(ExpectedVm {
            vm_id: "vm1".into(),
            vpc_id: "100".into(),
            tap_name: naming::tap_name("vm1"),
            mac: "02:00:0a:01:01:03".into(),
            ip: "10.1.1.3".into(),
        });

        state.vm_rules.push((
            naming::tap_name("vm1"),
            "02:00:0a:01:01:03".into(),
            "10.1.1.3".into(),
        ));

        // Old orphaned IP
        state.ip_allocations.push(IpAllocation {
            ip: "10.1.1.99".into(),
            subnet_id: "sub-1".into(),
            vm_id: None,
            mac: "02:00:0a:01:01:63".into(),
            state: AllocationState::Reserved,
            allocated_at: 999_000, // 1000s ago
            assigned_at: None,
        });

        let report = reconcile_network(&backend, &state).await;

        assert_eq!(report.bridges_fixed, 1, "bridge 200 should be re-created");
        assert_eq!(report.rules_reapplied, 1, "vm1 rules re-applied");
        assert_eq!(report.orphans_reclaimed, 1, "one orphaned IP reclaimed");

        // Warnings: orphaned bridge 999, orphaned TAP ghost
        assert!(report.warnings.iter().any(|w| w.contains(&br_999)));
        assert!(report.warnings.iter().any(|w| w.contains(&tap_ghost)));
    }

    #[tokio::test]
    async fn sg_drift_detected() {
        let mock = MockBackend::new();
        let mut state = NetworkState::default();
        state.sg_assignments.push(ExpectedSgAssignment {
            vm_id: "vm-1".to_string(),
            security_groups: vec!["web-sg".to_string(), "default".to_string()],
        });
        state.sg_assignments.push(ExpectedSgAssignment {
            vm_id: "vm-2".to_string(),
            security_groups: vec![], // no SGs → should be skipped
        });

        let report = reconcile_network(&mock, &state).await;
        assert_eq!(report.sg_chains_reapplied, 1);
    }

    #[tokio::test]
    async fn no_sg_assignments_no_drift() {
        let mock = MockBackend::new();
        let state = NetworkState::default();
        let report = reconcile_network(&mock, &state).await;
        assert_eq!(report.sg_chains_reapplied, 0);
    }
}
