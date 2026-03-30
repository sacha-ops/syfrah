//! TAP and veth management for VM and container networking.
//!
//! - **TAP** interfaces (`syftap-{vm_id}`) connect Cloud Hypervisor VMs to the
//!   VPC bridge.
//! - **veth pairs** (`syfve-{vm_id}-h` / `syfve-{vm_id}-c`) connect crun
//!   containers — the host side is attached to the bridge and the container
//!   side is moved into the container network namespace.
//!
//! All operations are idempotent: creating an interface that already exists is
//! a no-op rather than an error.

use crate::backend::NetworkBackend;
use crate::error::{OverlayError, Result};

/// Canonical TAP name for a given VM.
pub fn tap_name(vm_id: &str) -> String {
    format!("syftap-{vm_id}")
}

/// Canonical veth host-side name for a given VM.
pub fn veth_host_name(vm_id: &str) -> String {
    format!("syfve-{vm_id}-h")
}

/// Canonical veth container-side name for a given VM.
pub fn veth_container_name(vm_id: &str) -> String {
    format!("syfve-{vm_id}-c")
}

/// Create a TAP device and bring it up.
///
/// Idempotent: if the TAP already exists this is a no-op.
pub fn create_tap(backend: &dyn NetworkBackend, vm_id: &str) -> Result<String> {
    let name = tap_name(vm_id);
    if backend.interface_exists(&name)? {
        return Ok(name);
    }
    backend.create_tap(&name)?;
    Ok(name)
}

/// Delete a TAP device.
///
/// Idempotent: if the TAP does not exist this is a no-op.
pub fn delete_tap(backend: &dyn NetworkBackend, vm_id: &str) -> Result<()> {
    let name = tap_name(vm_id);
    if !backend.interface_exists(&name)? {
        return Ok(());
    }
    backend.delete_tap(&name)
}

/// Create a veth pair for a container and bring both ends up.
///
/// Returns `(host_name, container_name)`.
///
/// Idempotent: if the host side already exists this is a no-op.
pub fn create_veth_pair(backend: &dyn NetworkBackend, vm_id: &str) -> Result<(String, String)> {
    let host = veth_host_name(vm_id);
    let container = veth_container_name(vm_id);
    if backend.interface_exists(&host)? {
        return Ok((host, container));
    }
    backend.create_veth_pair(&host, &container)?;
    Ok((host, container))
}

/// Delete a veth pair.
///
/// Deleting the host side automatically removes the peer.
///
/// Idempotent: if the host side does not exist this is a no-op.
pub fn delete_veth_pair(backend: &dyn NetworkBackend, vm_id: &str) -> Result<()> {
    let host = veth_host_name(vm_id);
    if !backend.interface_exists(&host)? {
        return Ok(());
    }
    backend.delete_veth_pair(&host)
}

/// Attach the host-side interface (TAP or veth) to a VPC bridge.
///
/// The bridge must exist; otherwise an error is returned.
pub fn attach_to_bridge(backend: &dyn NetworkBackend, interface: &str, bridge: &str) -> Result<()> {
    if !backend.interface_exists(bridge)? {
        return Err(OverlayError::BridgeNotFound(bridge.to_string()));
    }
    backend.attach_to_bridge(interface, bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[test]
    fn create_tap_correct_name() {
        let backend = MockBackend::new();
        let name = create_tap(&backend, "abc123").unwrap();
        assert_eq!(name, "syftap-abc123");
        assert!(backend.has_interface("syftap-abc123"));

        let log = backend.call_log();
        assert!(log.iter().any(|c| c == "create_tap(syftap-abc123)"));
    }

    #[test]
    fn create_tap_idempotent() {
        let backend = MockBackend::new();
        create_tap(&backend, "abc123").unwrap();
        // Second call should be a no-op.
        create_tap(&backend, "abc123").unwrap();

        let log = backend.call_log();
        let create_count = log.iter().filter(|c| c.starts_with("create_tap")).count();
        assert_eq!(create_count, 1, "create_tap should be called only once");
    }

    #[test]
    fn create_veth_pair_both_ends() {
        let backend = MockBackend::new();
        let (host, container) = create_veth_pair(&backend, "ctr42").unwrap();
        assert_eq!(host, "syfve-ctr42-h");
        assert_eq!(container, "syfve-ctr42-c");
        assert!(backend.has_interface("syfve-ctr42-h"));
        assert!(backend.has_interface("syfve-ctr42-c"));

        let log = backend.call_log();
        assert!(log
            .iter()
            .any(|c| c == "create_veth_pair(syfve-ctr42-h, syfve-ctr42-c)"));
    }

    #[test]
    fn create_veth_pair_idempotent() {
        let backend = MockBackend::new();
        create_veth_pair(&backend, "ctr42").unwrap();
        create_veth_pair(&backend, "ctr42").unwrap();

        let log = backend.call_log();
        let count = log
            .iter()
            .filter(|c| c.starts_with("create_veth_pair"))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn attach_to_bridge_host_side() {
        let backend = MockBackend::new();
        // Create the bridge first so it exists in the mock.
        backend.create_bridge("syfbr-100").unwrap();

        let tap = create_tap(&backend, "vm1").unwrap();
        attach_to_bridge(&backend, &tap, "syfbr-100").unwrap();

        let log = backend.call_log();
        assert!(log
            .iter()
            .any(|c| c == "attach_to_bridge(syftap-vm1, syfbr-100)"));
    }

    #[test]
    fn attach_to_bridge_missing_bridge() {
        let backend = MockBackend::new();
        let tap = create_tap(&backend, "vm1").unwrap();
        let err = attach_to_bridge(&backend, &tap, "syfbr-999").unwrap_err();
        assert!(
            matches!(err, OverlayError::BridgeNotFound(ref b) if b == "syfbr-999"),
            "expected BridgeNotFound, got {err:?}"
        );
    }

    #[test]
    fn delete_tap_cleanup() {
        let backend = MockBackend::new();
        create_tap(&backend, "vm1").unwrap();
        assert!(backend.has_interface("syftap-vm1"));

        delete_tap(&backend, "vm1").unwrap();
        assert!(!backend.has_interface("syftap-vm1"));

        let log = backend.call_log();
        assert!(log.iter().any(|c| c == "delete_tap(syftap-vm1)"));
    }

    #[test]
    fn delete_tap_idempotent() {
        let backend = MockBackend::new();
        // Deleting a non-existent TAP is a no-op.
        delete_tap(&backend, "gone").unwrap();
        let log = backend.call_log();
        assert!(
            !log.iter().any(|c| c.starts_with("delete_tap")),
            "should not call delete_tap when interface does not exist"
        );
    }

    #[test]
    fn delete_veth_both_ends_removed() {
        let backend = MockBackend::new();
        create_veth_pair(&backend, "ctr1").unwrap();
        assert!(backend.has_interface("syfve-ctr1-h"));
        assert!(backend.has_interface("syfve-ctr1-c"));

        delete_veth_pair(&backend, "ctr1").unwrap();
        // Host side removed via delete_veth_pair — the real kernel also
        // removes the peer automatically. The mock removes the host side.
        assert!(!backend.has_interface("syfve-ctr1-h"));

        let log = backend.call_log();
        assert!(log.iter().any(|c| c == "delete_veth_pair(syfve-ctr1-h)"));
    }

    #[test]
    fn delete_veth_idempotent() {
        let backend = MockBackend::new();
        delete_veth_pair(&backend, "gone").unwrap();
        let log = backend.call_log();
        assert!(!log.iter().any(|c| c.starts_with("delete_veth_pair")));
    }
}
