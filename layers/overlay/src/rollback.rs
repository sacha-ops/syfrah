//! Partial-create rollback for VM network setup.
//!
//! When `vm create` fails mid-way (e.g. TAP creation fails after IPAM
//! allocation), the [`NetworkRollback`] struct tracks what was done and
//! reverses each step in reverse order. Rollback itself is best-effort:
//! errors are logged but never propagated.

use tracing::warn;

use crate::backend::NetworkBackend;

/// Tracks network resources allocated during VM creation so they can be
/// cleaned up if a later step fails.
///
/// Each field is set as the corresponding step succeeds. On error the
/// caller invokes [`NetworkRollback::rollback`] which undoes every
/// recorded step in reverse order.
#[derive(Debug, Default)]
pub struct NetworkRollback {
    /// (subnet_id, ip) — IP allocated from IPAM.
    pub ip_allocated: Option<(String, String)>,
    /// TAP device name.
    pub tap_created: Option<String>,
    /// Bridge name (only set when this VM caused the bridge to be created).
    pub bridge_created: Option<String>,
    /// TAP device name for which nftables rules were applied.
    pub nft_applied: Option<String>,
    /// (bridge, subnet_cidr) — NAT/masquerade rule applied.
    pub nat_applied: Option<(String, String)>,
    /// (vpc_id, vm_id) — VM placement stored in FDB/placement table.
    pub placement_stored: Option<(String, String)>,
}

/// Callback to release an IP allocation. The overlay layer does not own
/// IPAM directly, so the caller provides this closure.
pub type IpReleaseCallback = Box<dyn FnOnce(&str, &str) + Send>;

impl NetworkRollback {
    /// Create a new empty rollback tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that an IP was allocated from IPAM.
    pub fn set_ip_allocated(&mut self, subnet_id: impl Into<String>, ip: impl Into<String>) {
        self.ip_allocated = Some((subnet_id.into(), ip.into()));
    }

    /// Record that a TAP device was created.
    pub fn set_tap_created(&mut self, tap_name: impl Into<String>) {
        self.tap_created = Some(tap_name.into());
    }

    /// Record that a bridge was created for this VM (first VM in VPC on node).
    pub fn set_bridge_created(&mut self, bridge_name: impl Into<String>) {
        self.bridge_created = Some(bridge_name.into());
    }

    /// Record that nftables rules were applied for this VM's TAP.
    pub fn set_nft_applied(&mut self, tap_name: impl Into<String>) {
        self.nft_applied = Some(tap_name.into());
    }

    /// Record that NAT/masquerade was applied for this subnet.
    pub fn set_nat_applied(&mut self, bridge: impl Into<String>, subnet_cidr: impl Into<String>) {
        self.nat_applied = Some((bridge.into(), subnet_cidr.into()));
    }

    /// Record that a VM placement was stored.
    pub fn set_placement_stored(&mut self, vpc_id: impl Into<String>, vm_id: impl Into<String>) {
        self.placement_stored = Some((vpc_id.into(), vm_id.into()));
    }

    /// Roll back all recorded steps in reverse order.
    ///
    /// This is best-effort: each step that fails is logged but does not
    /// prevent subsequent steps from being attempted. The optional
    /// `ip_release` callback is invoked (if provided) when an IP
    /// allocation needs to be released — the overlay crate does not own
    /// IPAM, so the caller supplies the release logic.
    pub async fn rollback(
        self,
        backend: &dyn NetworkBackend,
        ip_release: Option<IpReleaseCallback>,
    ) {
        // Reverse order of creation: placement → NAT → nft → TAP → bridge → IP.

        if let Some((vpc_id, vm_id)) = &self.placement_stored {
            warn!(
                vpc_id,
                vm_id, "rollback: placement stored (caller must remove from store)"
            );
        }

        if let Some((bridge, subnet_cidr)) = &self.nat_applied {
            if let Err(e) = backend.remove_nat(bridge, subnet_cidr).await {
                warn!(bridge, subnet_cidr, error = %e, "rollback: failed to remove NAT");
            }
        }

        if let Some(tap) = &self.nft_applied {
            if let Err(e) = backend.remove_vm_rules(tap).await {
                warn!(tap, error = %e, "rollback: failed to remove nftables rules");
            }
        }

        if let Some(tap) = &self.tap_created {
            if let Err(e) = backend.delete_tap(tap).await {
                warn!(tap, error = %e, "rollback: failed to delete TAP");
            }
        }

        if let Some(bridge) = &self.bridge_created {
            if let Err(e) = backend.delete_bridge(bridge).await {
                warn!(bridge, error = %e, "rollback: failed to delete bridge");
            }
        }

        if let Some((subnet_id, ip)) = &self.ip_allocated {
            if let Some(release_fn) = ip_release {
                release_fn(subnet_id, ip);
            } else {
                warn!(
                    subnet_id,
                    ip, "rollback: IP allocated but no release callback provided"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;
    use std::sync::{Arc, Mutex};

    /// Helper: run the full VM network setup sequence, injecting a failure
    /// at `create_tap`, then verify rollback cleaned everything up.
    #[tokio::test]
    async fn partial_create_rollback() {
        let backend = MockBackend::new();
        backend.set_fail("create_tap");

        let released = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let released_clone = Arc::clone(&released);

        let mut rb = NetworkRollback::new();

        // Step 1: IPAM allocation succeeds
        rb.set_ip_allocated("subnet-1", "10.1.1.3");

        // Step 2: Bridge creation succeeds
        backend.create_bridge("syfbr-100").await.unwrap();
        rb.set_bridge_created("syfbr-100");

        // Step 3: TAP creation fails
        let tap_result = backend.create_tap("syftap-vm1").await;
        assert!(tap_result.is_err(), "create_tap should have failed");

        // Rollback
        let release_cb: IpReleaseCallback = Box::new(move |subnet, ip| {
            released_clone
                .lock()
                .unwrap()
                .push((subnet.to_string(), ip.to_string()));
        });
        rb.rollback(&backend, Some(release_cb)).await;

        // Verify: bridge was deleted
        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c == "delete_bridge(syfbr-100)"),
            "bridge should be rolled back, got: {calls:?}"
        );

        // Verify: IP was released
        let released_ips = released.lock().unwrap();
        assert_eq!(released_ips.len(), 1);
        assert_eq!(
            released_ips[0],
            ("subnet-1".to_string(), "10.1.1.3".to_string())
        );
    }

    /// Verify that IPAM release is called when TAP creation fails.
    #[tokio::test]
    async fn ip_released_on_tap_fail() {
        let backend = MockBackend::new();
        backend.set_fail("create_tap");

        let released = Arc::new(Mutex::new(false));
        let released_clone = Arc::clone(&released);

        let mut rb = NetworkRollback::new();
        rb.set_ip_allocated("subnet-1", "10.1.1.5");

        // TAP fails
        let tap_result = backend.create_tap("syftap-vm2").await;
        assert!(tap_result.is_err());

        let release_cb: IpReleaseCallback = Box::new(move |_subnet, _ip| {
            *released_clone.lock().unwrap() = true;
        });
        rb.rollback(&backend, Some(release_cb)).await;

        assert!(*released.lock().unwrap(), "IPAM release must be called");
    }

    /// If a bridge was created for this VM and rollback happens, the bridge
    /// must be deleted so it does not leak.
    #[tokio::test]
    async fn bridge_not_leaked() {
        let backend = MockBackend::new();
        backend.set_fail("create_tap");

        let mut rb = NetworkRollback::new();
        rb.set_ip_allocated("subnet-1", "10.1.1.3");

        // Bridge created for this VM (first VM in VPC on this node)
        backend.create_bridge("syfbr-200").await.unwrap();
        rb.set_bridge_created("syfbr-200");

        // TAP fails
        let tap_result = backend.create_tap("syftap-vm3").await;
        assert!(tap_result.is_err());

        rb.rollback(&backend, None).await;

        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c == "delete_bridge(syfbr-200)"),
            "bridge must be deleted on rollback, got: {calls:?}"
        );

        // Verify no TAP deletion attempted (it was never created)
        assert!(
            !calls.iter().any(|c| c.starts_with("delete_tap(")),
            "should not attempt to delete a TAP that was never created"
        );
    }

    /// Rollback with all steps recorded should clean up everything in
    /// reverse order.
    #[tokio::test]
    async fn full_rollback_reverse_order() {
        let backend = MockBackend::new();

        let mut rb = NetworkRollback::new();
        rb.set_ip_allocated("subnet-1", "10.1.1.3");
        rb.set_bridge_created("syfbr-100");
        rb.set_tap_created("syftap-vm1");
        rb.set_nft_applied("syftap-vm1");
        rb.set_nat_applied("syfbr-100", "10.1.1.0/24");
        rb.set_placement_stored("100", "vm1");

        let released = Arc::new(Mutex::new(false));
        let released_clone = Arc::clone(&released);
        let release_cb: IpReleaseCallback = Box::new(move |_s, _i| {
            *released_clone.lock().unwrap() = true;
        });

        rb.rollback(&backend, Some(release_cb)).await;

        let calls = backend.calls();
        // Expect reverse order: remove_nat, remove_vm_rules, delete_tap, delete_bridge
        assert_eq!(calls.len(), 4, "expected 4 backend calls, got: {calls:?}");
        assert!(calls[0].starts_with("remove_nat("), "first: remove NAT");
        assert!(
            calls[1].starts_with("remove_vm_rules("),
            "second: remove nft rules"
        );
        assert!(calls[2].starts_with("delete_tap("), "third: delete TAP");
        assert!(
            calls[3].starts_with("delete_bridge("),
            "fourth: delete bridge"
        );

        // IP release callback was called
        assert!(*released.lock().unwrap());
    }

    /// Rollback is best-effort: if one cleanup step fails, the rest still run.
    #[tokio::test]
    async fn rollback_best_effort_continues_on_error() {
        let backend = MockBackend::new();
        // delete_tap will fail (error before recording the call)
        backend.set_fail("delete_tap");

        let mut rb = NetworkRollback::new();
        rb.set_ip_allocated("subnet-1", "10.1.1.3");
        rb.set_tap_created("syftap-vm1");
        rb.set_bridge_created("syfbr-100");

        let released = Arc::new(Mutex::new(false));
        let released_clone = Arc::clone(&released);
        let release_cb: IpReleaseCallback = Box::new(move |_s, _i| {
            *released_clone.lock().unwrap() = true;
        });

        rb.rollback(&backend, Some(release_cb)).await;

        // delete_tap fails silently (best-effort), but delete_bridge and
        // IP release should still proceed.
        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("delete_bridge(")),
            "delete_bridge should still run after delete_tap failure, got: {calls:?}"
        );
        assert!(
            *released.lock().unwrap(),
            "IP release should run despite TAP delete failure"
        );
    }

    /// Empty rollback does nothing.
    #[tokio::test]
    async fn empty_rollback_is_noop() {
        let backend = MockBackend::new();
        let rb = NetworkRollback::new();
        rb.rollback(&backend, None).await;
        assert!(backend.calls().is_empty());
    }
}
