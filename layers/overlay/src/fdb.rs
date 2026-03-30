//! Static FDB (Forwarding Database) and ARP proxy management.
//!
//! All FDB entries are static — the control plane knows where every VM is.
//! No flood-and-learn, no broadcast, no MAC learning races.

use crate::backend::NetworkBackend;
use crate::error::{OverlayError, Result};

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
}
