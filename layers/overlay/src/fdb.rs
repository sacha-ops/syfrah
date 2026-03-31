//! Static FDB (Forwarding Database) and ARP proxy management.
//!
//! All FDB entries are static — the control plane knows where every VM is.
//! No flood-and-learn, no broadcast, no MAC learning races.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::backend::NetworkBackend;
use crate::error::{OverlayError, Result};
use crate::naming;

/// Action carried by a VM placement announcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementAction {
    Add,
    Remove,
}

/// Announcement broadcast when a VM is created or deleted.
///
/// Each node in the VPC uses this to update its local FDB and ARP proxy tables.
/// Also used by [`rebuild_fdb`] to re-populate entries on daemon restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmPlacement {
    pub vpc_id: String,
    pub vm_id: String,
    pub vm_mac: String,
    pub vm_ip: String,
    pub subnet_id: String,
    pub hosting_node: String,
    pub action: PlacementAction,
}

/// Summary returned by [`rebuild_fdb`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildSummary {
    /// Number of FDB + ARP entries successfully rebuilt.
    pub rebuilt: usize,
    /// Placements on the local node (no FDB needed).
    pub skipped_local: usize,
    /// Placements where the FDB/ARP add failed.
    pub errors: usize,
}

/// Rebuild FDB tables from persisted `vm_placements`.
///
/// Called at daemon startup after reconnecting VMs. Iterates all placements
/// and adds FDB + ARP proxy entries for every remote VM (where `hosting_node`
/// differs from `local_node`). Local placements are skipped. Failures are
/// counted but do not abort the rebuild — the function is best-effort so
/// that a single stale placement does not block the entire daemon startup.
pub async fn rebuild_fdb(
    backend: &dyn NetworkBackend,
    placements: &[VmPlacement],
    local_node: &str,
) -> Result<RebuildSummary> {
    let mut summary = RebuildSummary::default();

    for p in placements {
        if p.hosting_node == local_node {
            summary.skipped_local += 1;
            continue;
        }

        let bridge = naming::bridge_name(&p.vpc_id);
        let vxlan = naming::vxlan_name(&p.vpc_id);

        match add_fdb_entry(backend, &bridge, &p.vm_mac, &p.hosting_node).await {
            Ok(()) => {}
            Err(e) => {
                warn!(
                    vm_id = %p.vm_id, vpc_id = %p.vpc_id,
                    error = %e, "failed to rebuild FDB entry"
                );
                summary.errors += 1;
                continue;
            }
        }

        match add_arp_proxy(backend, &vxlan, &p.vm_ip, &p.vm_mac).await {
            Ok(()) => {}
            Err(e) => {
                warn!(
                    vm_id = %p.vm_id, vpc_id = %p.vpc_id,
                    error = %e, "failed to rebuild ARP proxy entry"
                );
                summary.errors += 1;
                continue;
            }
        }

        summary.rebuilt += 1;
    }

    info!(
        rebuilt = summary.rebuilt,
        skipped_local = summary.skipped_local,
        errors = summary.errors,
        "FDB rebuild complete"
    );

    Ok(summary)
}

/// Naming convention: VXLAN interface for a given bridge.
///
/// Bridge name: `syfb-{hash}` -> VXLAN name: `syfx-{hash}`.
fn vxlan_name_from_bridge(bridge: &str) -> Result<String> {
    let suffix = bridge
        .strip_prefix(naming::BRIDGE_PREFIX)
        .ok_or_else(|| OverlayError::InterfaceNotFound(bridge.to_string()))?;
    Ok(format!("{}{suffix}", naming::VXLAN_PREFIX))
}

/// Add a static FDB entry for a remote VM.
pub async fn add_fdb_entry(
    backend: &dyn NetworkBackend,
    bridge: &str,
    mac: &str,
    vtep: &str,
) -> Result<()> {
    let _vxlan = vxlan_name_from_bridge(bridge)?;
    backend.add_fdb_entry(bridge, mac, vtep).await
}

/// Remove a static FDB entry.
pub async fn remove_fdb_entry(backend: &dyn NetworkBackend, bridge: &str, mac: &str) -> Result<()> {
    let _vxlan = vxlan_name_from_bridge(bridge)?;
    backend.remove_fdb_entry(bridge, mac).await
}

/// Add an ARP proxy entry on a VXLAN interface.
pub async fn add_arp_proxy(
    backend: &dyn NetworkBackend,
    vxlan: &str,
    ip: &str,
    mac: &str,
) -> Result<()> {
    backend.add_arp_proxy(vxlan, ip, mac).await
}

/// Register a VM placement on a remote node.
///
/// Adds both the FDB entry (MAC -> VTEP) and the ARP proxy (IP -> MAC).
pub async fn register_remote_vm(
    backend: &dyn NetworkBackend,
    bridge: &str,
    vxlan: &str,
    mac: &str,
    ip: &str,
    vtep: &str,
) -> Result<()> {
    add_fdb_entry(backend, bridge, mac, vtep).await?;
    add_arp_proxy(backend, vxlan, ip, mac).await?;
    Ok(())
}

/// Remove an ARP proxy entry from a VXLAN interface.
pub async fn remove_arp_proxy(backend: &dyn NetworkBackend, vxlan: &str, ip: &str) -> Result<()> {
    backend.remove_arp_proxy(vxlan, ip).await
}

/// Synchronise local FDB and ARP proxy tables from a VM placement announcement.
///
/// - Skips if the placement refers to the local node (local VMs don't need FDB entries).
/// - On `Add`: creates both FDB and ARP proxy entries.
/// - On `Remove`: removes both FDB and ARP proxy entries.
/// - Idempotent: adding an existing entry or removing a non-existent one does not error.
pub async fn sync_placement(
    backend: &dyn NetworkBackend,
    placement: &VmPlacement,
    local_node: &str,
) -> Result<()> {
    // Don't add FDB entries for VMs running on this node.
    if placement.hosting_node == local_node {
        return Ok(());
    }

    let bridge = naming::bridge_name(&placement.vpc_id);
    let vxlan = naming::vxlan_name(&placement.vpc_id);

    match placement.action {
        PlacementAction::Add => {
            add_fdb_entry(backend, &bridge, &placement.vm_mac, &placement.hosting_node).await?;
            add_arp_proxy(backend, &vxlan, &placement.vm_ip, &placement.vm_mac).await?;
        }
        PlacementAction::Remove => {
            remove_fdb_entry(backend, &bridge, &placement.vm_mac).await?;
            remove_arp_proxy(backend, &vxlan, &placement.vm_ip).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    fn bridge_100() -> String {
        naming::bridge_name("100")
    }
    fn vxlan_100() -> String {
        naming::vxlan_name("100")
    }
    const MAC: &str = "02:00:0a:00:01:05";
    const IP: &str = "10.0.1.5";
    const VTEP: &str = "fd12:3456:7800::2";

    #[tokio::test]
    async fn add_fdb_entry_correct_mac_and_vtep() {
        let backend = MockBackend::new();
        let bridge = bridge_100();
        add_fdb_entry(&backend, &bridge, MAC, VTEP).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], format!("add_fdb_entry({bridge}, {MAC}, {VTEP})"));
    }

    #[tokio::test]
    async fn add_fdb_entry_rejects_invalid_bridge_name() {
        let backend = MockBackend::new();
        let result = add_fdb_entry(&backend, "not-a-bridge", MAC, VTEP).await;
        assert!(result.is_err());
        assert!(backend.calls().is_empty());
    }

    #[tokio::test]
    async fn remove_fdb_entry_cleanup() {
        let backend = MockBackend::new();
        let bridge = bridge_100();
        add_fdb_entry(&backend, &bridge, MAC, VTEP).await.unwrap();
        remove_fdb_entry(&backend, &bridge, MAC).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], format!("remove_fdb_entry({bridge}, {MAC})"));
    }

    #[tokio::test]
    async fn add_arp_proxy_entry() {
        let backend = MockBackend::new();
        let vxlan = vxlan_100();
        add_arp_proxy(&backend, &vxlan, IP, MAC).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], format!("add_arp_proxy({vxlan}, {IP}, {MAC})"));
    }

    #[tokio::test]
    async fn register_remote_vm_adds_fdb_and_arp() {
        let backend = MockBackend::new();
        let bridge = bridge_100();
        let vxlan = vxlan_100();
        register_remote_vm(&backend, &bridge, &vxlan, MAC, IP, VTEP)
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].starts_with("add_fdb_entry("));
        assert!(calls[1].starts_with("add_arp_proxy("));
    }

    fn make_placement(action: PlacementAction, hosting_node: &str) -> VmPlacement {
        VmPlacement {
            vpc_id: "100".to_string(),
            vm_id: "vm-1".to_string(),
            vm_mac: MAC.to_string(),
            vm_ip: IP.to_string(),
            subnet_id: "sub-1".to_string(),
            hosting_node: hosting_node.to_string(),
            action,
        }
    }

    #[tokio::test]
    async fn add_fdb_on_announce() {
        let backend = MockBackend::new();
        let placement = make_placement(PlacementAction::Add, VTEP);
        sync_placement(&backend, &placement, "fd12:3456:7800::1")
            .await
            .unwrap();

        let calls = backend.calls();
        let bridge = bridge_100();
        let vxlan = vxlan_100();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], format!("add_fdb_entry({bridge}, {MAC}, {VTEP})"));
        assert_eq!(calls[1], format!("add_arp_proxy({vxlan}, {IP}, {MAC})"));
    }

    #[tokio::test]
    async fn remove_fdb_on_remove() {
        let backend = MockBackend::new();
        let placement = make_placement(PlacementAction::Remove, VTEP);
        sync_placement(&backend, &placement, "fd12:3456:7800::1")
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        let bridge = bridge_100();
        let vxlan = vxlan_100();
        assert_eq!(calls[0], format!("remove_fdb_entry({bridge}, {MAC})"));
        assert_eq!(calls[1], format!("remove_arp_proxy({vxlan}, {IP})"));
    }

    #[tokio::test]
    async fn ignore_self_node() {
        let backend = MockBackend::new();
        let local = "fd12:3456:7800::1";
        let placement = make_placement(PlacementAction::Add, local);
        sync_placement(&backend, &placement, local).await.unwrap();

        assert!(
            backend.calls().is_empty(),
            "should not touch FDB for local VMs"
        );
    }

    #[tokio::test]
    async fn idempotent_apply() {
        let backend = MockBackend::new();
        let placement = make_placement(PlacementAction::Add, VTEP);

        // Apply twice — the mock always returns Ok, simulating idempotent ops.
        sync_placement(&backend, &placement, "fd12:3456:7800::1")
            .await
            .unwrap();
        sync_placement(&backend, &placement, "fd12:3456:7800::1")
            .await
            .unwrap();

        // Both calls succeed without error; 4 total calls (2 per sync).
        assert_eq!(backend.calls().len(), 4);
    }

    // ── rebuild_fdb tests ─────────────────────────────────────────────

    fn placement(vpc: &str, vm: &str, mac: &str, ip: &str, node: &str) -> VmPlacement {
        VmPlacement {
            vpc_id: vpc.to_string(),
            vm_id: vm.to_string(),
            vm_mac: mac.to_string(),
            vm_ip: ip.to_string(),
            subnet_id: "sub-1".to_string(),
            hosting_node: node.to_string(),
            action: PlacementAction::Add,
        }
    }

    #[tokio::test]
    async fn rebuild_fdb_from_table() {
        let backend = MockBackend::new();
        let placements = vec![
            placement("100", "vm-1", "02:00:0a:01:01:03", "10.1.1.3", "node-2"),
            placement("100", "vm-2", "02:00:0a:01:01:04", "10.1.1.4", "node-3"),
            placement("200", "vm-3", "02:00:0a:02:01:03", "10.2.1.3", "node-2"),
            // local — should be skipped
            placement("100", "vm-local", "02:00:0a:01:01:05", "10.1.1.5", "node-1"),
        ];

        let summary = rebuild_fdb(&backend, &placements, "node-1").await.unwrap();

        assert_eq!(summary.rebuilt, 3);
        assert_eq!(summary.skipped_local, 1);
        assert_eq!(summary.errors, 0);

        let calls = backend.calls();
        // 3 remote placements x 2 calls each (FDB + ARP)
        assert_eq!(calls.len(), 6);
        let bridge = bridge_100();
        let vxlan = vxlan_100();
        assert!(calls[0].contains(&format!("add_fdb_entry({bridge}")));
        assert!(calls[1].contains(&format!("add_arp_proxy({vxlan}")));
    }

    #[tokio::test]
    async fn skip_dead_placements() {
        let backend = MockBackend::new();
        backend.set_fail("add_fdb_entry");

        let placements = vec![
            placement("100", "vm-1", "02:00:0a:01:01:03", "10.1.1.3", "node-2"),
            placement("100", "vm-2", "02:00:0a:01:01:04", "10.1.1.4", "node-3"),
        ];

        let summary = rebuild_fdb(&backend, &placements, "node-1").await.unwrap();

        // Both should be counted as errors, but the function continues
        assert_eq!(summary.errors, 2);
        assert_eq!(summary.rebuilt, 0);
        assert_eq!(summary.skipped_local, 0);
    }

    #[tokio::test]
    async fn reconcile_with_kernel() {
        // Calling rebuild_fdb twice should work identically (idempotent).
        let backend = MockBackend::new();
        let placements = vec![placement(
            "100",
            "vm-1",
            "02:00:0a:01:01:03",
            "10.1.1.3",
            "node-2",
        )];

        let s1 = rebuild_fdb(&backend, &placements, "node-1").await.unwrap();
        let s2 = rebuild_fdb(&backend, &placements, "node-1").await.unwrap();

        assert_eq!(s1, s2);
        assert_eq!(s1.rebuilt, 1);

        // Backend should have 4 calls total (2 per rebuild)
        let calls = backend.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0], calls[2], "identical FDB call on second rebuild");
        assert_eq!(calls[1], calls[3], "identical ARP call on second rebuild");
    }
}
