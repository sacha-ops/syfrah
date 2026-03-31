//! TAP and veth management for VM and container networking.
//!
//! - **TAP** interfaces connect Cloud Hypervisor VMs to the VPC bridge.
//! - **veth pairs** connect crun containers — the host side is attached to
//!   the bridge and the container side is moved into the container network
//!   namespace.
//!
//! All operations are idempotent: creating an interface that already exists is
//! a no-op rather than an error.

use crate::backend::NetworkBackend;
use crate::error::Result;
use crate::naming;

/// Canonical TAP name for a given VM.
pub fn tap_name(vm_id: &str) -> String {
    naming::tap_name(vm_id)
}

/// Canonical veth host-side name for a given VM.
pub fn veth_host_name(vm_id: &str) -> String {
    naming::veth_host_name(vm_id)
}

/// Canonical veth container-side name for a given VM.
pub fn veth_container_name(vm_id: &str) -> String {
    naming::veth_container_name(vm_id)
}

/// Create a TAP device and bring it up.
///
/// Idempotent: the backend handles already-existing interfaces gracefully.
pub async fn create_tap(backend: &dyn NetworkBackend, vm_id: &str) -> Result<String> {
    let name = tap_name(vm_id);
    backend.create_tap(&name).await?;
    Ok(name)
}

/// Delete a TAP device.
///
/// Idempotent: the backend handles non-existent interfaces gracefully.
pub async fn delete_tap(backend: &dyn NetworkBackend, vm_id: &str) -> Result<()> {
    let name = tap_name(vm_id);
    backend.delete_tap(&name).await
}

/// Create a veth pair for a container and bring both ends up.
///
/// Returns `(host_name, container_name)`.
///
/// Idempotent: the backend handles already-existing interfaces gracefully.
pub async fn create_veth_pair(
    backend: &dyn NetworkBackend,
    vm_id: &str,
) -> Result<(String, String)> {
    let host = veth_host_name(vm_id);
    let container = veth_container_name(vm_id);
    backend.create_veth_pair(&host, &container).await?;
    Ok((host, container))
}

/// Attach the host-side interface (TAP or veth) to a VPC bridge.
pub async fn attach_to_bridge(
    backend: &dyn NetworkBackend,
    interface: &str,
    bridge: &str,
) -> Result<()> {
    backend.attach_to_bridge(interface, bridge).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[tokio::test]
    async fn create_tap_correct_name() {
        let backend = MockBackend::new();
        let name = create_tap(&backend, "abc123").await.unwrap();
        assert_eq!(name, naming::tap_name("abc123"));

        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("create_tap({})", naming::tap_name("abc123"))));
    }

    #[tokio::test]
    async fn create_veth_pair_both_ends() {
        let backend = MockBackend::new();
        let (host, container) = create_veth_pair(&backend, "ctr42").await.unwrap();
        assert_eq!(host, naming::veth_host_name("ctr42"));
        assert_eq!(container, naming::veth_container_name("ctr42"));

        let calls = backend.calls();
        assert!(calls.iter().any(|c| c
            == &format!(
                "create_veth_pair({}, {})",
                naming::veth_host_name("ctr42"),
                naming::veth_container_name("ctr42")
            )));
    }

    #[tokio::test]
    async fn attach_to_bridge_host_side() {
        let backend = MockBackend::new();
        let tap = create_tap(&backend, "vm1").await.unwrap();
        let bridge = naming::bridge_name("100");
        attach_to_bridge(&backend, &tap, &bridge).await.unwrap();

        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("attach_to_bridge({}, {})", naming::tap_name("vm1"), bridge)));
    }

    #[tokio::test]
    async fn delete_tap_cleanup() {
        let backend = MockBackend::new();
        create_tap(&backend, "vm1").await.unwrap();
        delete_tap(&backend, "vm1").await.unwrap();

        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("delete_tap({})", naming::tap_name("vm1"))));
    }
}
