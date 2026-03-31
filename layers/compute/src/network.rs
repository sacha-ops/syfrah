//! Network cleanup for VM deletion.
//!
//! When a VM is deleted, we need to tear down the network resources that were
//! created for it: FDB entry, IPAM allocation, TAP device, nftables rules,
//! and potentially the bridge/VXLAN if no more VMs remain on it.
//!
//! All cleanup is best-effort: if an individual step fails, we log a warning
//! and continue. The reconciliation loop (Step 10) will catch leftovers.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use syfrah_overlay::NetworkBackend;

// ── NetworkInfo ─────────────────────────────────────────────────────────

/// Network metadata attached to a VM at creation time.
///
/// This stores everything needed to tear down the VM's network on delete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInfo {
    /// VPC ID (used for bridge/VXLAN naming via the `syfrah_overlay::naming` module).
    pub vpc_id: String,
    /// Subnet ID (used for IPAM release).
    pub subnet_id: String,
    /// Subnet CIDR (e.g., "10.0.1.0/24") — needed for IPAM release.
    pub subnet_cidr: String,
    /// Allocated IP address (e.g., "10.0.1.3").
    pub ip: String,
    /// Derived MAC address (e.g., "02:00:0a:00:01:03").
    pub mac: String,
    /// TAP device name (hash-based, e.g., "syft-a1b2c3d4").
    pub tap_name: String,
    /// Hosting node fabric address (for FDB removal).
    pub hosting_node: String,
}

// ── NetworkCleanup ──────────────────────────────────────────────────────

/// Handles network teardown when a VM is deleted.
///
/// All operations are best-effort: failures are logged as warnings but do
/// not abort the delete. The reconciliation loop will catch leftovers.
pub struct NetworkCleanup<B: NetworkBackend + ?Sized> {
    backend: Arc<B>,
}

impl<B: NetworkBackend + ?Sized> NetworkCleanup<B> {
    /// Create a new `NetworkCleanup` with the given network backend.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Perform full network cleanup for a deleted VM.
    ///
    /// Steps (all best-effort):
    /// 1. Remove FDB entry (VmPlacement)
    /// 2. Release IP (IPAM)
    /// 3. Delete TAP device
    /// 4. Remove nftables rules
    /// 5. Check bridge: if no more TAPs, remove gateway IP. If no gateway
    ///    IPs remain, delete bridge + VXLAN + NAT rules.
    ///
    /// `remaining_vms_on_bridge` is the count of VMs still using this bridge
    /// (excluding the VM being deleted). If 0, the bridge is cleaned up.
    ///
    /// `ipam_release` is a callback to release the IP in IPAM. This avoids
    /// a direct dependency on the org crate's store at runtime — the caller
    /// passes the release function.
    pub async fn cleanup(
        &self,
        info: &NetworkInfo,
        remaining_vms_on_bridge: usize,
        ipam_release: Option<Box<dyn FnOnce() -> Result<(), String> + Send>>,
    ) -> CleanupResult {
        let mut result = CleanupResult::default();
        let bridge = syfrah_overlay::naming::bridge_name(&info.vpc_id);
        let vxlan = syfrah_overlay::naming::vxlan_name(&info.vpc_id);

        // 1. Remove FDB entry
        if let Err(e) = self.backend.remove_fdb_entry(&bridge, &info.mac).await {
            warn!(
                vm_ip = %info.ip, mac = %info.mac,
                error = %e, "failed to remove FDB entry (best-effort)"
            );
            result.fdb_error = Some(e.to_string());
        } else {
            result.fdb_removed = true;
        }

        // Also remove ARP proxy entry
        if let Err(e) = self.backend.remove_arp_proxy(&vxlan, &info.ip).await {
            warn!(
                vm_ip = %info.ip,
                error = %e, "failed to remove ARP proxy (best-effort)"
            );
        }

        // 2. Release IP (IPAM)
        if let Some(release_fn) = ipam_release {
            match release_fn() {
                Ok(()) => {
                    result.ip_released = true;
                }
                Err(e) => {
                    warn!(
                        vm_ip = %info.ip, subnet = %info.subnet_id,
                        error = %e, "failed to release IP (best-effort)"
                    );
                    result.ipam_error = Some(e);
                }
            }
        }

        // 3. Delete TAP device
        if let Err(e) = self.backend.delete_tap(&info.tap_name).await {
            warn!(
                tap = %info.tap_name,
                error = %e, "failed to delete TAP (best-effort)"
            );
            result.tap_error = Some(e.to_string());
        } else {
            result.tap_removed = true;
        }

        // 4. Remove nftables rules
        if let Err(e) = self.backend.remove_vm_rules(&info.tap_name).await {
            warn!(
                tap = %info.tap_name,
                error = %e, "failed to remove nftables rules (best-effort)"
            );
            result.nft_error = Some(e.to_string());
        } else {
            result.nft_removed = true;
        }

        // 5. Bridge cleanup: if no more VMs on this bridge, tear it all down
        if remaining_vms_on_bridge == 0 {
            // Derive gateway IP: subnet base with .1 suffix (e.g., 10.0.1.0/24 -> 10.0.1.1)
            let gateway_ip = info
                .subnet_cidr
                .split('/')
                .next()
                .and_then(|base| {
                    let parts: Vec<&str> = base.rsplitn(2, '.').collect();
                    if parts.len() == 2 {
                        Some(format!("{}.1", parts[1]))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| info.ip.clone());

            // Remove subnet gateway IP from bridge
            if let Err(e) = self.backend.remove_bridge_ip(&bridge, &gateway_ip).await {
                warn!(
                    bridge = %bridge,
                    error = %e, "failed to remove bridge gateway IP (best-effort)"
                );
            }

            // Remove NAT rules
            if let Err(e) = self.backend.remove_nat(&bridge, &info.subnet_cidr).await {
                warn!(
                    bridge = %bridge,
                    error = %e, "failed to remove NAT rules (best-effort)"
                );
            }

            // Delete VXLAN
            if let Err(e) = self.backend.delete_vxlan(&vxlan).await {
                warn!(
                    vxlan = %vxlan,
                    error = %e, "failed to delete VXLAN (best-effort)"
                );
            }

            // Delete bridge
            if let Err(e) = self.backend.delete_bridge(&bridge).await {
                warn!(
                    bridge = %bridge,
                    error = %e, "failed to delete bridge (best-effort)"
                );
                result.bridge_error = Some(e.to_string());
            } else {
                result.bridge_deleted = true;
            }
        }

        result
    }
}

// ── CleanupResult ───────────────────────────────────────────────────────

/// Summary of what was cleaned up (or failed) during network teardown.
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    /// FDB entry was successfully removed.
    pub fdb_removed: bool,
    /// IP was successfully released from IPAM.
    pub ip_released: bool,
    /// TAP device was successfully deleted.
    pub tap_removed: bool,
    /// nftables rules were successfully removed.
    pub nft_removed: bool,
    /// Bridge was deleted (only when no VMs remain).
    pub bridge_deleted: bool,
    /// Error removing FDB entry.
    pub fdb_error: Option<String>,
    /// Error releasing IP.
    pub ipam_error: Option<String>,
    /// Error deleting TAP.
    pub tap_error: Option<String>,
    /// Error removing nftables rules.
    pub nft_error: Option<String>,
    /// Error deleting bridge.
    pub bridge_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use syfrah_overlay::MockBackend;

    fn sample_info() -> NetworkInfo {
        NetworkInfo {
            vpc_id: "100".to_string(),
            subnet_id: "default/frontend".to_string(),
            subnet_cidr: "10.0.1.0/24".to_string(),
            ip: "10.0.1.3".to_string(),
            mac: "02:00:0a:00:01:03".to_string(),
            tap_name: syfrah_overlay::naming::tap_name("web-1"),
            hosting_node: "fd00::1".to_string(),
        }
    }

    #[tokio::test]
    async fn delete_vm_releases_ip() {
        let backend = Arc::new(MockBackend::new());
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        let ip_released = Arc::new(std::sync::Mutex::new(false));
        let ip_released_clone = Arc::clone(&ip_released);
        let release_fn: Box<dyn FnOnce() -> Result<(), String> + Send> = Box::new(move || {
            *ip_released_clone.lock().unwrap() = true;
            Ok(())
        });

        let result = cleanup.cleanup(&info, 1, Some(release_fn)).await;
        assert!(result.ip_released, "IPAM release should have been called");
        assert!(*ip_released.lock().unwrap(), "release callback was invoked");
    }

    #[tokio::test]
    async fn tap_removed() {
        let backend = Arc::new(MockBackend::new());
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        let result = cleanup.cleanup(&info, 1, None).await;
        assert!(result.tap_removed, "TAP should be removed");

        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c == &format!("delete_tap({})", syfrah_overlay::naming::tap_name("web-1"))),
            "delete_tap should have been called"
        );
    }

    #[tokio::test]
    async fn fdb_removed() {
        let backend = Arc::new(MockBackend::new());
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        let result = cleanup.cleanup(&info, 1, None).await;
        assert!(result.fdb_removed, "FDB entry should be removed");

        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c.starts_with(&format!(
                "remove_fdb_entry({}",
                syfrah_overlay::naming::bridge_name("100")
            ))),
            "remove_fdb_entry should have been called"
        );
    }

    #[tokio::test]
    async fn bridge_deleted_when_empty() {
        let backend = Arc::new(MockBackend::new());
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        // remaining_vms_on_bridge = 0 => bridge should be deleted
        let result = cleanup.cleanup(&info, 0, None).await;
        assert!(
            result.bridge_deleted,
            "bridge should be deleted when no VMs remain"
        );

        let calls = backend.calls();
        let br = syfrah_overlay::naming::bridge_name("100");
        let vx = syfrah_overlay::naming::vxlan_name("100");
        assert!(
            calls.iter().any(|c| c == &format!("delete_bridge({br})")),
            "delete_bridge should have been called"
        );
        assert!(
            calls.iter().any(|c| c == &format!("delete_vxlan({vx})")),
            "delete_vxlan should have been called"
        );
        assert!(
            calls
                .iter()
                .any(|c| c == &format!("remove_nat({br}, 10.0.1.0/24)")),
            "remove_nat should have been called"
        );
    }

    #[tokio::test]
    async fn bridge_kept_when_other_vms() {
        let backend = Arc::new(MockBackend::new());
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        // remaining_vms_on_bridge = 2 => bridge stays
        let result = cleanup.cleanup(&info, 2, None).await;
        assert!(
            !result.bridge_deleted,
            "bridge should NOT be deleted when other VMs exist"
        );

        let calls = backend.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("delete_bridge(")),
            "delete_bridge should NOT have been called"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("delete_vxlan(")),
            "delete_vxlan should NOT have been called"
        );
    }

    #[tokio::test]
    async fn nft_failure_does_not_abort() {
        let backend = Arc::new(MockBackend::new());
        backend.set_fail("remove_vm_rules");
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        // Even though nft fails, the rest should still work
        let result = cleanup.cleanup(&info, 1, None).await;
        assert!(!result.nft_removed, "nft removal should have failed");
        assert!(result.nft_error.is_some(), "nft error should be recorded");
        assert!(result.tap_removed, "TAP should still be removed");
        assert!(result.fdb_removed, "FDB should still be removed");
    }

    #[tokio::test]
    async fn ipam_failure_does_not_abort() {
        let backend = Arc::new(MockBackend::new());
        let cleanup = NetworkCleanup::new(Arc::clone(&backend));
        let info = sample_info();

        let release_fn: Box<dyn FnOnce() -> Result<(), String> + Send> =
            Box::new(|| Err("IPAM store unavailable".to_string()));

        let result = cleanup.cleanup(&info, 1, Some(release_fn)).await;
        assert!(!result.ip_released, "IPAM release should have failed");
        assert!(result.ipam_error.is_some(), "IPAM error should be recorded");
        // Other steps still succeed
        assert!(result.tap_removed, "TAP should still be removed");
        assert!(result.fdb_removed, "FDB should still be removed");
    }
}
