//! Static FDB (Forwarding Database) and ARP proxy management.
//!
//! All FDB entries are static — the control plane knows where every VM is.
//! No flood-and-learn, no broadcast, no MAC learning races.

use serde::{Deserialize, Serialize};

use crate::backend::NetworkBackend;
use crate::error::{OverlayError, Result};

/// Action carried by a VM placement announcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementAction {
    Add,
    Remove,
}

/// Announcement broadcast when a VM is created or deleted.
///
/// Each node in the VPC uses this to update its local FDB and ARP proxy tables.
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

/// Naming convention: VXLAN interface for a given bridge.
///
/// Bridge name: `syfbr-{vpc_id}` -> VXLAN name: `syfvx-{vpc_id}`.
fn vxlan_name_from_bridge(bridge: &str) -> Result<String> {
    let suffix = bridge
        .strip_prefix("syfbr-")
        .ok_or_else(|| OverlayError::InterfaceNotFound(bridge.to_string()))?;
    Ok(format!("syfvx-{suffix}"))
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

    let bridge = format!("syfbr-{}", placement.vpc_id);
    let vxlan = format!("syfvx-{}", placement.vpc_id);

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

    const BRIDGE: &str = "syfbr-100";
    const VXLAN: &str = "syfvx-100";
    const MAC: &str = "02:00:0a:00:01:05";
    const IP: &str = "10.0.1.5";
    const VTEP: &str = "fd12:3456:7800::2";

    #[tokio::test]
    async fn add_fdb_entry_correct_mac_and_vtep() {
        let backend = MockBackend::new();
        add_fdb_entry(&backend, BRIDGE, MAC, VTEP).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], format!("add_fdb_entry({BRIDGE}, {MAC}, {VTEP})"));
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
        add_fdb_entry(&backend, BRIDGE, MAC, VTEP).await.unwrap();
        remove_fdb_entry(&backend, BRIDGE, MAC).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], format!("remove_fdb_entry({BRIDGE}, {MAC})"));
    }

    #[tokio::test]
    async fn add_arp_proxy_entry() {
        let backend = MockBackend::new();
        add_arp_proxy(&backend, VXLAN, IP, MAC).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], format!("add_arp_proxy({VXLAN}, {IP}, {MAC})"));
    }

    #[tokio::test]
    async fn register_remote_vm_adds_fdb_and_arp() {
        let backend = MockBackend::new();
        register_remote_vm(&backend, BRIDGE, VXLAN, MAC, IP, VTEP)
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
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], format!("add_fdb_entry(syfbr-100, {MAC}, {VTEP})"));
        assert_eq!(calls[1], format!("add_arp_proxy(syfvx-100, {IP}, {MAC})"));
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
        assert_eq!(calls[0], format!("remove_fdb_entry(syfbr-100, {MAC})"));
        assert_eq!(calls[1], format!("remove_arp_proxy(syfvx-100, {IP})"));
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
}
