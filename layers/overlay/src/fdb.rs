//! Static FDB (Forwarding Database) and ARP proxy management.
//!
//! All FDB entries are static — the control plane knows where every VM is.
//! No flood-and-learn, no broadcast, no MAC learning races.
//!
//! ## FDB entries
//!
//! When a VM is created on a remote node, every other node adds a static
//! FDB entry pointing the VM's MAC to the remote node's VXLAN VTEP:
//!
//! ```text
//! bridge fdb add {mac} dev {vxlan_of_bridge} dst {vtep}
//! ```
//!
//! ## ARP proxy
//!
//! VXLAN interfaces run in proxy mode. The control plane populates
//! neighbor entries from IPAM so that ARP requests are answered locally:
//!
//! ```text
//! ip neigh add {ip} lladdr {mac} dev {vxlan} nud permanent
//! ```

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::backend::{MacAddr, NetworkBackend};
use crate::error::{OverlayError, Result};

/// Naming convention: VXLAN interface for a given bridge.
///
/// Bridge name: `syfbr-{vpc_id}` -> VXLAN name: `syfvx-{vpc_id}`.
fn vxlan_name_from_bridge(bridge: &str) -> Result<String> {
    let suffix = bridge
        .strip_prefix("syfbr-")
        .ok_or_else(|| OverlayError::VxlanNotFound(bridge.to_string()))?;
    Ok(format!("syfvx-{suffix}"))
}

/// Add a static FDB entry for a remote VM.
///
/// This tells the bridge: "MAC `mac` is reachable via VXLAN tunnel
/// endpoint `vtep`". The underlying command is:
///
/// ```text
/// bridge fdb add {mac} dev {vxlan_of_bridge} dst {vtep}
/// ```
pub fn add_fdb_entry(
    backend: &dyn NetworkBackend,
    bridge: &str,
    mac: MacAddr,
    vtep: Ipv6Addr,
) -> Result<()> {
    let _vxlan = vxlan_name_from_bridge(bridge)?;
    backend.add_fdb_entry(bridge, mac, vtep)
}

/// Remove a static FDB entry.
///
/// Called when a VM is deleted or migrated away from a node.
///
/// ```text
/// bridge fdb del {mac} dev {vxlan_of_bridge}
/// ```
pub fn remove_fdb_entry(backend: &dyn NetworkBackend, bridge: &str, mac: MacAddr) -> Result<()> {
    let _vxlan = vxlan_name_from_bridge(bridge)?;
    backend.remove_fdb_entry(bridge, mac)
}

/// Add an ARP proxy entry on a VXLAN interface.
///
/// This allows the VXLAN interface to answer ARP requests locally
/// without flooding them across the overlay:
///
/// ```text
/// ip neigh add {ip} lladdr {mac} dev {vxlan} nud permanent
/// ```
pub fn add_arp_proxy(
    backend: &dyn NetworkBackend,
    vxlan: &str,
    ip: Ipv4Addr,
    mac: MacAddr,
) -> Result<()> {
    backend.add_arp_proxy(vxlan, ip, mac)
}

/// Remove an ARP proxy entry from a VXLAN interface.
///
/// ```text
/// ip neigh del {ip} dev {vxlan}
/// ```
pub fn remove_arp_proxy(backend: &dyn NetworkBackend, vxlan: &str, ip: Ipv4Addr) -> Result<()> {
    backend.remove_arp_proxy(vxlan, ip)
}

/// Register a VM placement on a remote node.
///
/// Adds both the FDB entry (MAC -> VTEP) and the ARP proxy (IP -> MAC)
/// so the local node can forward traffic to the remote VM without
/// any broadcast or flood-and-learn.
pub fn register_remote_vm(
    backend: &dyn NetworkBackend,
    bridge: &str,
    vxlan: &str,
    mac: MacAddr,
    ip: Ipv4Addr,
    vtep: Ipv6Addr,
) -> Result<()> {
    add_fdb_entry(backend, bridge, mac, vtep)?;
    add_arp_proxy(backend, vxlan, ip, mac)?;
    Ok(())
}

/// Unregister a remote VM placement.
///
/// Removes both the FDB entry and the ARP proxy entry.
pub fn unregister_remote_vm(
    backend: &dyn NetworkBackend,
    bridge: &str,
    vxlan: &str,
    mac: MacAddr,
    ip: Ipv4Addr,
) -> Result<()> {
    remove_fdb_entry(backend, bridge, mac)?;
    remove_arp_proxy(backend, vxlan, ip)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockBackend, MockCall};

    const BRIDGE: &str = "syfbr-100";
    const VXLAN: &str = "syfvx-100";

    fn test_mac() -> MacAddr {
        MacAddr::parse("02:00:0a:00:01:05").unwrap()
    }

    fn test_ip() -> Ipv4Addr {
        "10.0.1.5".parse().unwrap()
    }

    fn remote_vtep() -> Ipv6Addr {
        "fd12:3456:7800::2".parse().unwrap()
    }

    // ── add_fdb_entry ───────────────────────────────────────────────

    #[test]
    fn add_fdb_entry_correct_mac_and_vtep() {
        let backend = MockBackend::new();
        let mac = test_mac();
        let vtep = remote_vtep();

        add_fdb_entry(&backend, BRIDGE, mac, vtep).unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            MockCall::AddFdbEntry {
                bridge: BRIDGE.to_string(),
                mac,
                vtep,
            }
        );
    }

    #[test]
    fn add_fdb_entry_rejects_invalid_bridge_name() {
        let backend = MockBackend::new();
        let result = add_fdb_entry(&backend, "not-a-bridge", test_mac(), remote_vtep());
        assert!(result.is_err());
        assert!(backend.calls().is_empty());
    }

    // ── remove_fdb_entry ────────────────────────────────────────────

    #[test]
    fn remove_fdb_entry_cleanup() {
        let backend = MockBackend::new();
        let mac = test_mac();

        // Add then remove.
        add_fdb_entry(&backend, BRIDGE, mac, remote_vtep()).unwrap();
        remove_fdb_entry(&backend, BRIDGE, mac).unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            MockCall::RemoveFdbEntry {
                bridge: BRIDGE.to_string(),
                mac,
            }
        );
    }

    // ── add_arp_proxy ───────────────────────────────────────────────

    #[test]
    fn add_arp_proxy_correct_ip_mac_pair() {
        let backend = MockBackend::new();
        let mac = test_mac();
        let ip = test_ip();

        add_arp_proxy(&backend, VXLAN, ip, mac).unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            MockCall::AddArpProxy {
                vxlan: VXLAN.to_string(),
                ip,
                mac,
            }
        );
    }

    // ── remove_arp_proxy ────────────────────────────────────────────

    #[test]
    fn remove_arp_proxy_cleanup() {
        let backend = MockBackend::new();
        let ip = test_ip();
        let mac = test_mac();

        add_arp_proxy(&backend, VXLAN, ip, mac).unwrap();
        remove_arp_proxy(&backend, VXLAN, ip).unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            MockCall::RemoveArpProxy {
                vxlan: VXLAN.to_string(),
                ip,
            }
        );
    }

    // ── fdb_for_remote_node ─────────────────────────────────────────

    #[test]
    fn fdb_for_remote_node() {
        let backend = MockBackend::new();
        let mac = test_mac();
        let ip = test_ip();
        let vtep: Ipv6Addr = "fd12:3456:7800::99".parse().unwrap();

        // Simulate registering a VM on a remote node.
        register_remote_vm(&backend, BRIDGE, VXLAN, mac, ip, vtep).unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);

        // FDB entry points to remote fabric IPv6.
        assert_eq!(
            calls[0],
            MockCall::AddFdbEntry {
                bridge: BRIDGE.to_string(),
                mac,
                vtep,
            }
        );

        // ARP proxy maps IP to MAC on the VXLAN interface.
        assert_eq!(
            calls[1],
            MockCall::AddArpProxy {
                vxlan: VXLAN.to_string(),
                ip,
                mac,
            }
        );
    }

    #[test]
    fn unregister_remote_vm_cleanup() {
        let backend = MockBackend::new();
        let mac = test_mac();
        let ip = test_ip();
        let vtep = remote_vtep();

        register_remote_vm(&backend, BRIDGE, VXLAN, mac, ip, vtep).unwrap();
        backend.clear();

        unregister_remote_vm(&backend, BRIDGE, VXLAN, mac, ip).unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            MockCall::RemoveFdbEntry {
                bridge: BRIDGE.to_string(),
                mac,
            }
        );
        assert_eq!(
            calls[1],
            MockCall::RemoveArpProxy {
                vxlan: VXLAN.to_string(),
                ip,
            }
        );
    }

    #[test]
    fn vxlan_name_derived_from_bridge() {
        assert_eq!(vxlan_name_from_bridge("syfbr-100").unwrap(), "syfvx-100");
        assert_eq!(vxlan_name_from_bridge("syfbr-abc").unwrap(), "syfvx-abc");
        assert!(vxlan_name_from_bridge("br0").is_err());
    }
}
