//! Daemon restart network recovery.
//!
//! On daemon restart, kernel state may diverge from redb state:
//! - Bridges/VXLANs may have survived the restart (kernel keeps them)
//! - nftables rules are lost on reboot (not persisted by the kernel)
//! - FDB entries are lost on reboot
//! - Orphaned interfaces may exist (in kernel but not in redb)
//! - Missing interfaces may need re-creation (in redb but not in kernel)
//!
//! The [`recover_network`] function reconciles both directions and returns
//! a [`RecoveryReport`] summarizing what was done.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::backend::NetworkBackend;
use crate::fdb::{add_arp_proxy, add_fdb_entry};
use crate::naming;
use crate::vxlan::VXLAN_PORT;

/// A VPC descriptor used during recovery. Mirrors the essential fields
/// from `syfrah_org::Vpc` without creating a crate dependency.
#[derive(Debug, Clone)]
pub struct RecoveryVpc {
    /// VPC identifier (used in bridge/VXLAN naming via the `naming` module).
    pub id: String,
    /// VXLAN Network Identifier.
    pub vni: u32,
}

/// A subnet descriptor used during recovery.
#[derive(Debug, Clone)]
pub struct RecoverySubnet {
    /// The VPC this subnet belongs to.
    pub vpc_id: String,
    /// Subnet CIDR (e.g. "10.1.1.0/24").
    pub cidr: String,
    /// Gateway IP (e.g. "10.1.1.1").
    pub gateway: String,
}

/// A VM placement descriptor used during recovery.
#[derive(Debug, Clone)]
pub struct RecoveryPlacement {
    pub vpc_id: String,
    pub vm_id: String,
    pub vm_mac: String,
    pub vm_ip: String,
    pub hosting_node: String,
}

/// Summary of what the recovery function did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    /// Bridges that were re-created (missing from kernel, present in redb).
    pub bridges_recovered: usize,
    /// VXLAN interfaces that were re-created.
    pub vxlans_recovered: usize,
    /// nftables rulesets re-applied for VMs on this node.
    pub nft_reapplied: usize,
    /// FDB + ARP proxy entries rebuilt from placements.
    pub fdb_rebuilt: usize,
    /// Orphaned interfaces deleted (in kernel but not in redb).
    pub orphans_cleaned: usize,
}

/// Recover network state after a daemon restart.
///
/// Scans kernel interfaces, compares with the expected state derived from
/// `vpcs`, `subnets`, and `placements`, then reconciles:
///
/// 1. Re-create missing bridges and attach gateway IPs
/// 2. Re-create missing VXLAN interfaces and attach to bridges
/// 3. Re-apply nftables rules for all local VMs (rules do not survive reboot)
/// 4. Re-populate FDB and ARP proxy tables from placement records
/// 5. Delete orphaned interfaces (in kernel but not in redb)
///
/// All operations are best-effort: a single failure does not abort recovery.
pub async fn recover_network(
    backend: &dyn NetworkBackend,
    vpcs: &[RecoveryVpc],
    subnets: &[RecoverySubnet],
    placements: &[RecoveryPlacement],
    local_node: &str,
    local_ip: &str,
) -> RecoveryReport {
    let mut report = RecoveryReport::default();

    // ── 1. Discover kernel interfaces ─────────────────────────────────
    let kernel_bridges = backend
        .list_interfaces(naming::BRIDGE_PREFIX)
        .await
        .unwrap_or_default();
    let kernel_vxlans = backend
        .list_interfaces(naming::VXLAN_PREFIX)
        .await
        .unwrap_or_default();
    let kernel_taps = backend
        .list_interfaces(naming::TAP_PREFIX)
        .await
        .unwrap_or_default();
    let kernel_peers = backend
        .list_interfaces(naming::PEER_PREFIX)
        .await
        .unwrap_or_default();

    let kernel_bridge_set: HashSet<&str> = kernel_bridges.iter().map(|s| s.as_str()).collect();
    let kernel_vxlan_set: HashSet<&str> = kernel_vxlans.iter().map(|s| s.as_str()).collect();

    // ── 2. Determine expected state from redb ─────────────────────────

    // VPCs that have at least one local placement need a bridge + VXLAN.
    let local_vpc_ids: HashSet<&str> = placements
        .iter()
        .filter(|p| p.hosting_node == local_node)
        .map(|p| p.vpc_id.as_str())
        .collect();

    let expected_bridges: HashSet<String> = local_vpc_ids
        .iter()
        .map(|id| naming::bridge_name(id))
        .collect();

    let expected_vxlans: HashSet<String> = local_vpc_ids
        .iter()
        .map(|id| naming::vxlan_name(id))
        .collect();

    // Expected TAPs: local placements -> naming::tap_name(vm_id)
    let expected_taps: HashSet<String> = placements
        .iter()
        .filter(|p| p.hosting_node == local_node)
        .map(|p| naming::tap_name(&p.vm_id))
        .collect();

    // ── 3. Re-create missing bridges ──────────────────────────────────
    for vpc in vpcs {
        let bridge = naming::bridge_name(&vpc.id);
        if !expected_bridges.contains(&bridge) {
            continue;
        }
        if kernel_bridge_set.contains(bridge.as_str()) {
            // Bridge already exists in kernel — keep it.
            continue;
        }

        info!(bridge = %bridge, "recovering missing bridge");
        if let Err(e) = backend.create_bridge(&bridge).await {
            warn!(bridge = %bridge, error = %e, "failed to recover bridge");
            continue;
        }

        // Add gateway IPs for subnets in this VPC.
        for subnet in subnets.iter().filter(|s| s.vpc_id == vpc.id) {
            let prefix_len = subnet
                .cidr
                .split('/')
                .nth(1)
                .and_then(|p| p.parse::<u8>().ok())
                .unwrap_or(24);
            if let Err(e) = backend
                .add_bridge_ip(&bridge, &subnet.gateway, prefix_len)
                .await
            {
                warn!(
                    bridge = %bridge, gateway = %subnet.gateway,
                    error = %e, "failed to add gateway IP during recovery"
                );
            }
        }

        report.bridges_recovered += 1;
    }

    // ── 4. Re-create missing VXLANs ───────────────────────────────────
    for vpc in vpcs {
        let vxlan = naming::vxlan_name(&vpc.id);
        let bridge = naming::bridge_name(&vpc.id);

        if !expected_vxlans.contains(&vxlan) {
            continue;
        }
        if kernel_vxlan_set.contains(vxlan.as_str()) {
            continue;
        }

        info!(vxlan = %vxlan, vni = vpc.vni, "recovering missing VXLAN");
        if let Err(e) = backend
            .create_vxlan(&vxlan, vpc.vni, local_ip, VXLAN_PORT)
            .await
        {
            warn!(vxlan = %vxlan, error = %e, "failed to recover VXLAN");
            continue;
        }

        if let Err(e) = backend.attach_to_bridge(&vxlan, &bridge).await {
            warn!(
                vxlan = %vxlan, bridge = %bridge,
                error = %e, "failed to attach recovered VXLAN to bridge"
            );
        }

        report.vxlans_recovered += 1;
    }

    // ── 5. Re-apply nftables rules ────────────────────────────────────
    // nftables rules do not survive reboot — re-apply for every local VM.
    for p in placements.iter().filter(|p| p.hosting_node == local_node) {
        let tap = naming::tap_name(&p.vm_id);

        if let Err(e) = backend.apply_vm_rules(&tap, &p.vm_mac, &p.vm_ip).await {
            warn!(
                vm_id = %p.vm_id, tap = %tap,
                error = %e, "failed to re-apply nftables rules"
            );
            continue;
        }

        report.nft_reapplied += 1;
    }

    // Re-apply NAT for subnets that have local VMs.
    for subnet in subnets {
        if local_vpc_ids.contains(subnet.vpc_id.as_str()) {
            let bridge = naming::bridge_name(&subnet.vpc_id);
            if let Err(e) = backend.apply_nat(&bridge, &subnet.cidr).await {
                warn!(
                    bridge = %bridge, subnet = %subnet.cidr,
                    error = %e, "failed to re-apply NAT during recovery"
                );
            }
        }
    }

    // ── 6. Re-populate FDB from placements ────────────────────────────
    for p in placements.iter().filter(|p| p.hosting_node != local_node) {
        let bridge = naming::bridge_name(&p.vpc_id);
        let vxlan = naming::vxlan_name(&p.vpc_id);

        // Only rebuild FDB for VPCs where we have local VMs too.
        if !local_vpc_ids.contains(p.vpc_id.as_str()) {
            continue;
        }

        match add_fdb_entry(backend, &bridge, &p.vm_mac, &p.hosting_node).await {
            Ok(()) => {}
            Err(e) => {
                warn!(
                    vm_id = %p.vm_id, vpc_id = %p.vpc_id,
                    error = %e, "failed to rebuild FDB entry during recovery"
                );
                continue;
            }
        }

        match add_arp_proxy(backend, &vxlan, &p.vm_ip, &p.vm_mac).await {
            Ok(()) => {}
            Err(e) => {
                warn!(
                    vm_id = %p.vm_id, vpc_id = %p.vpc_id,
                    error = %e, "failed to rebuild ARP proxy during recovery"
                );
                continue;
            }
        }

        report.fdb_rebuilt += 1;
    }

    // ── 7. Clean orphaned interfaces ──────────────────────────────────
    // Bridges in kernel that are not expected.
    for bridge in &kernel_bridges {
        if !expected_bridges.contains(bridge) {
            info!(bridge = %bridge, "deleting orphaned bridge");
            if let Err(e) = backend.delete_bridge(bridge).await {
                warn!(bridge = %bridge, error = %e, "failed to delete orphaned bridge");
            } else {
                report.orphans_cleaned += 1;
            }
        }
    }

    // VXLANs in kernel that are not expected.
    for vxlan in &kernel_vxlans {
        if !expected_vxlans.contains(vxlan) {
            info!(vxlan = %vxlan, "deleting orphaned VXLAN");
            if let Err(e) = backend.delete_vxlan(vxlan).await {
                warn!(vxlan = %vxlan, error = %e, "failed to delete orphaned VXLAN");
            } else {
                report.orphans_cleaned += 1;
            }
        }
    }

    // TAPs in kernel that are not expected.
    for tap in &kernel_taps {
        if !expected_taps.contains(tap) {
            info!(tap = %tap, "deleting orphaned TAP");
            if let Err(e) = backend.delete_tap(tap).await {
                warn!(tap = %tap, error = %e, "failed to delete orphaned TAP");
            } else {
                report.orphans_cleaned += 1;
            }
        }
    }

    // Orphaned veth peers — we don't track them in the expected set for
    // simplicity, but peers without a matching bridge are orphans. For now
    // we skip peer cleanup and let future peering recovery handle it.
    let _ = kernel_peers;

    info!(
        bridges_recovered = report.bridges_recovered,
        vxlans_recovered = report.vxlans_recovered,
        nft_reapplied = report.nft_reapplied,
        fdb_rebuilt = report.fdb_rebuilt,
        orphans_cleaned = report.orphans_cleaned,
        "network recovery complete"
    );

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    const LOCAL_NODE: &str = "fd00::1";
    const LOCAL_IP: &str = "fd00::1";
    const REMOTE_NODE: &str = "fd00::2";

    fn vpc(id: &str, vni: u32) -> RecoveryVpc {
        RecoveryVpc {
            id: id.to_string(),
            vni,
        }
    }

    fn subnet(vpc_id: &str, cidr: &str, gateway: &str) -> RecoverySubnet {
        RecoverySubnet {
            vpc_id: vpc_id.to_string(),
            cidr: cidr.to_string(),
            gateway: gateway.to_string(),
        }
    }

    fn placement(vpc_id: &str, vm_id: &str, mac: &str, ip: &str, node: &str) -> RecoveryPlacement {
        RecoveryPlacement {
            vpc_id: vpc_id.to_string(),
            vm_id: vm_id.to_string(),
            vm_mac: mac.to_string(),
            vm_ip: ip.to_string(),
            hosting_node: node.to_string(),
        }
    }

    // ── bridges_survive_restart ───────────────────────────────────────
    // Existing bridges in kernel should be kept, not re-created.

    #[tokio::test]
    async fn bridges_survive_restart() {
        let backend = MockBackend::new();
        // Bridge already exists in the kernel.
        backend.set_interfaces(vec![naming::bridge_name("100"), naming::vxlan_name("100")]);

        let vpcs = vec![vpc("100", 100)];
        let subnets = vec![subnet("100", "10.1.1.0/24", "10.1.1.1")];
        let placements = vec![placement(
            "100",
            "vm-1",
            "02:00:0a:01:01:03",
            "10.1.1.3",
            LOCAL_NODE,
        )];

        let report =
            recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        // Bridge was already present — should NOT be re-created.
        assert_eq!(report.bridges_recovered, 0);
        // VXLAN was already present — should NOT be re-created.
        assert_eq!(report.vxlans_recovered, 0);

        let calls = backend.calls();
        // No create_bridge or create_vxlan calls expected.
        assert!(!calls.iter().any(|c| c.starts_with("create_bridge(")));
        assert!(!calls.iter().any(|c| c.starts_with("create_vxlan(")));
    }

    // ── taps_survive_restart ──────────────────────────────────────────
    // Existing TAPs in kernel that match redb state should not be deleted.

    #[tokio::test]
    async fn taps_survive_restart() {
        let backend = MockBackend::new();
        backend.set_interfaces(vec![
            naming::bridge_name("100"),
            naming::vxlan_name("100"),
            naming::tap_name("vm-1"),
        ]);

        let vpcs = vec![vpc("100", 100)];
        let subnets = vec![subnet("100", "10.1.1.0/24", "10.1.1.1")];
        let placements = vec![placement(
            "100",
            "vm-1",
            "02:00:0a:01:01:03",
            "10.1.1.3",
            LOCAL_NODE,
        )];

        let report =
            recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        // TAP exists and matches redb — should NOT be deleted.
        assert_eq!(report.orphans_cleaned, 0);

        let calls = backend.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c == &format!("delete_tap({})", naming::tap_name("vm-1"))),
            "TAP should not be deleted"
        );
    }

    // ── nftables_reapplied ────────────────────────────────────────────
    // nftables rules do not survive reboot and must be re-applied.

    #[tokio::test]
    async fn nftables_reapplied() {
        let backend = MockBackend::new();
        backend.set_interfaces(vec![
            naming::bridge_name("100"),
            naming::vxlan_name("100"),
            naming::tap_name("vm-1"),
            naming::tap_name("vm-2"),
        ]);

        let vpcs = vec![vpc("100", 100)];
        let subnets = vec![subnet("100", "10.1.1.0/24", "10.1.1.1")];
        let placements = vec![
            placement("100", "vm-1", "02:00:0a:01:01:03", "10.1.1.3", LOCAL_NODE),
            placement("100", "vm-2", "02:00:0a:01:01:04", "10.1.1.4", LOCAL_NODE),
        ];

        let report =
            recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        assert_eq!(report.nft_reapplied, 2);

        let calls = backend.calls();
        let apply_calls: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("apply_vm_rules("))
            .collect();
        assert_eq!(apply_calls.len(), 2);
        assert!(apply_calls[0].contains(&naming::tap_name("vm-1")));
        assert!(apply_calls[1].contains(&naming::tap_name("vm-2")));
    }

    // ── fdb_repopulated ───────────────────────────────────────────────
    // FDB entries for remote VMs must be rebuilt from placements.

    #[tokio::test]
    async fn fdb_repopulated() {
        let backend = MockBackend::new();
        backend.set_interfaces(vec![
            naming::bridge_name("100"),
            naming::vxlan_name("100"),
            naming::tap_name("vm-local"),
        ]);

        let vpcs = vec![vpc("100", 100)];
        let subnets = vec![subnet("100", "10.1.1.0/24", "10.1.1.1")];
        let placements = vec![
            // Local VM — should not get FDB entry
            placement(
                "100",
                "vm-local",
                "02:00:0a:01:01:05",
                "10.1.1.5",
                LOCAL_NODE,
            ),
            // Remote VM — needs FDB + ARP
            placement(
                "100",
                "vm-remote",
                "02:00:0a:01:01:06",
                "10.1.1.6",
                REMOTE_NODE,
            ),
        ];

        let report =
            recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        assert_eq!(report.fdb_rebuilt, 1);

        let calls = backend.calls();
        let fdb_calls: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("add_fdb_entry("))
            .collect();
        assert_eq!(fdb_calls.len(), 1);
        assert!(fdb_calls[0].contains("02:00:0a:01:01:06"));
        assert!(fdb_calls[0].contains(REMOTE_NODE));

        let arp_calls: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("add_arp_proxy("))
            .collect();
        assert_eq!(arp_calls.len(), 1);
        assert!(arp_calls[0].contains("10.1.1.6"));
    }

    // ── orphaned interfaces are cleaned ───────────────────────────────

    #[tokio::test]
    async fn orphans_deleted() {
        let backend = MockBackend::new();
        // Kernel has interfaces that are NOT in redb.
        backend.set_interfaces(vec![
            naming::bridge_name("orphan"),
            naming::vxlan_name("orphan"),
            naming::tap_name("deleted-vm"),
        ]);

        let vpcs: Vec<RecoveryVpc> = vec![];
        let subnets: Vec<RecoverySubnet> = vec![];
        let placements: Vec<RecoveryPlacement> = vec![];

        let report =
            recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        assert_eq!(report.orphans_cleaned, 3);

        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("delete_bridge({})", naming::bridge_name("orphan"))));
        assert!(calls
            .iter()
            .any(|c| c == &format!("delete_vxlan({})", naming::vxlan_name("orphan"))));
        assert!(calls
            .iter()
            .any(|c| c == &format!("delete_tap({})", naming::tap_name("deleted-vm"))));
    }

    // ── missing bridge is re-created ──────────────────────────────────

    #[tokio::test]
    async fn missing_bridge_recreated() {
        let backend = MockBackend::new();
        // Kernel has no interfaces at all (fresh reboot).
        backend.set_interfaces(vec![]);

        let vpcs = vec![vpc("200", 200)];
        let subnets = vec![subnet("200", "10.2.1.0/24", "10.2.1.1")];
        let placements = vec![placement(
            "200",
            "vm-1",
            "02:00:0a:02:01:03",
            "10.2.1.3",
            LOCAL_NODE,
        )];

        let report =
            recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        assert_eq!(report.bridges_recovered, 1);
        assert_eq!(report.vxlans_recovered, 1);

        let calls = backend.calls();
        let br200 = naming::bridge_name("200");
        let vx200 = naming::vxlan_name("200");
        assert!(calls
            .iter()
            .any(|c| c == &format!("create_bridge({br200})")));
        assert!(calls
            .iter()
            .any(|c| c == &format!("add_bridge_ip({br200}, 10.2.1.1, 24)")));
        assert!(calls
            .iter()
            .any(|c| c.starts_with(&format!("create_vxlan({vx200}"))));
        assert!(calls
            .iter()
            .any(|c| c == &format!("attach_to_bridge({vx200}, {br200})")));
    }

    // ── NAT rules re-applied for local subnets ────────────────────────

    #[tokio::test]
    async fn nat_reapplied() {
        let backend = MockBackend::new();
        backend.set_interfaces(vec![naming::bridge_name("100"), naming::vxlan_name("100")]);

        let vpcs = vec![vpc("100", 100)];
        let subnets = vec![
            subnet("100", "10.1.1.0/24", "10.1.1.1"),
            subnet("100", "10.1.2.0/24", "10.1.2.1"),
        ];
        let placements = vec![placement(
            "100",
            "vm-1",
            "02:00:0a:01:01:03",
            "10.1.1.3",
            LOCAL_NODE,
        )];

        recover_network(&backend, &vpcs, &subnets, &placements, LOCAL_NODE, LOCAL_IP).await;

        let calls = backend.calls();
        let nat_calls: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("apply_nat("))
            .collect();
        assert_eq!(nat_calls.len(), 2);
    }

    // ── empty state produces clean report ─────────────────────────────

    #[tokio::test]
    async fn empty_state_no_ops() {
        let backend = MockBackend::new();

        let report = recover_network(&backend, &[], &[], &[], LOCAL_NODE, LOCAL_IP).await;

        assert_eq!(report, RecoveryReport::default());
    }
}
