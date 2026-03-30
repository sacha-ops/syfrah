//! VXLAN interface management.
//!
//! One VXLAN interface per VPC per node: `syfvx-{vpc_id}`.
//! Created on-demand when the first VM in a VPC lands on this node.

use crate::backend::NetworkBackend;
use crate::error::Result;

/// Default VXLAN UDP destination port.
pub const VXLAN_PORT: u16 = 4789;

/// Derive the VXLAN interface name from a VPC ID.
pub fn vxlan_name(vpc_id: &str) -> String {
    format!("syfvx-{vpc_id}")
}

/// Derive the bridge name from a VPC ID.
pub fn bridge_name(vpc_id: &str) -> String {
    format!("syfbr-{vpc_id}")
}

/// Create a VXLAN interface for the given VPC and attach it to the VPC bridge.
///
/// Idempotent: creates only if not already present.
///
/// Steps:
/// 1. Create with `nolearning` + `proxy` flags, bring up.
/// 2. Attach to `syfbr-{vpc_id}`.
pub async fn ensure_vxlan(
    backend: &dyn NetworkBackend,
    vpc_id: &str,
    vni: u32,
    local_ip: &str,
) -> Result<()> {
    let name = vxlan_name(vpc_id);
    let bridge = bridge_name(vpc_id);

    tracing::info!(vxlan = %name, vni, %local_ip, "creating VXLAN interface");
    backend
        .create_vxlan(&name, vni, local_ip, VXLAN_PORT)
        .await?;

    tracing::info!(vxlan = %name, bridge = %bridge, "attaching VXLAN to VPC bridge");
    backend.attach_to_bridge(&name, &bridge).await?;

    Ok(())
}

/// Delete the VXLAN interface for a VPC.
///
/// Idempotent: if the interface does not exist, the backend handles it gracefully.
pub async fn remove_vxlan(backend: &dyn NetworkBackend, vpc_id: &str) -> Result<()> {
    let name = vxlan_name(vpc_id);

    tracing::info!(vxlan = %name, "deleting VXLAN interface");
    backend.delete_vxlan(&name).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[tokio::test]
    async fn create_vxlan() {
        let backend = MockBackend::new();

        ensure_vxlan(&backend, "vpc-100", 100, "fd00::1")
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], "create_vxlan(syfvx-vpc-100, 100, fd00::1, 4789)");
        assert_eq!(calls[1], "attach_to_bridge(syfvx-vpc-100, syfbr-vpc-100)");
    }

    #[tokio::test]
    async fn correct_vni() {
        let backend = MockBackend::new();

        ensure_vxlan(&backend, "vpc-42", 42, "fd00::1")
            .await
            .unwrap();

        let calls = backend.calls();
        assert!(calls[0].contains("42"));
    }

    #[tokio::test]
    async fn attach_to_bridge() {
        let backend = MockBackend::new();

        ensure_vxlan(&backend, "vpc-200", 200, "fd00::1")
            .await
            .unwrap();

        let attach_calls: Vec<_> = backend
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("attach_to_bridge("))
            .collect();
        assert_eq!(attach_calls.len(), 1);
        assert_eq!(
            attach_calls[0],
            "attach_to_bridge(syfvx-vpc-200, syfbr-vpc-200)"
        );
    }

    #[tokio::test]
    async fn delete_vxlan() {
        let backend = MockBackend::new();

        ensure_vxlan(&backend, "vpc-300", 300, "fd00::1")
            .await
            .unwrap();
        remove_vxlan(&backend, "vpc-300").await.unwrap();

        let delete_calls: Vec<_> = backend
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("delete_vxlan("))
            .collect();
        assert_eq!(delete_calls.len(), 1);
        assert_eq!(delete_calls[0], "delete_vxlan(syfvx-vpc-300)");
    }
}
