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
use crate::error::Result;

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
        assert_eq!(name, "syftap-abc123");

        let calls = backend.calls();
        assert!(calls.iter().any(|c| c == "create_tap(syftap-abc123)"));
    }

    #[tokio::test]
    async fn create_veth_pair_both_ends() {
        let backend = MockBackend::new();
        let (host, container) = create_veth_pair(&backend, "ctr42").await.unwrap();
        assert_eq!(host, "syfve-ctr42-h");
        assert_eq!(container, "syfve-ctr42-c");

        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == "create_veth_pair(syfve-ctr42-h, syfve-ctr42-c)"));
    }

    #[tokio::test]
    async fn attach_to_bridge_host_side() {
        let backend = MockBackend::new();
        let tap = create_tap(&backend, "vm1").await.unwrap();
        attach_to_bridge(&backend, &tap, "syfbr-100").await.unwrap();

        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == "attach_to_bridge(syftap-vm1, syfbr-100)"));
    }

    #[tokio::test]
    async fn delete_tap_cleanup() {
        let backend = MockBackend::new();
        create_tap(&backend, "vm1").await.unwrap();
        delete_tap(&backend, "vm1").await.unwrap();

        let calls = backend.calls();
        assert!(calls.iter().any(|c| c == "delete_tap(syftap-vm1)"));
    }
}
