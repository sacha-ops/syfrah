//! VXLAN interface management.
//!
//! One VXLAN interface per VPC per node.
//! Created on-demand when the first VM in a VPC lands on this node.

use crate::backend::NetworkBackend;
use crate::error::Result;
use crate::naming;

/// Default VXLAN UDP destination port.
pub const VXLAN_PORT: u16 = 4789;

/// Derive the VXLAN interface name from a VPC ID.
pub fn vxlan_name(vpc_id: &str) -> String {
    naming::vxlan_name(vpc_id)
}

/// Derive the bridge name from a VPC ID.
pub fn bridge_name(vpc_id: &str) -> String {
    naming::bridge_name(vpc_id)
}

/// Create a VXLAN interface for the given VPC and attach it to the VPC bridge.
///
/// Idempotent: creates only if not already present.
///
/// Steps:
/// 1. Create with `nolearning` + `proxy` flags, bring up.
/// 2. Attach to VPC bridge.
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
        let expected_vxlan = naming::vxlan_name("vpc-100");
        let expected_bridge = naming::bridge_name("vpc-100");
        assert_eq!(
            calls[0],
            format!("create_vxlan({expected_vxlan}, 100, fd00::1, 4789)")
        );
        assert_eq!(
            calls[1],
            format!("attach_to_bridge({expected_vxlan}, {expected_bridge})")
        );
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
        let expected_vxlan = naming::vxlan_name("vpc-200");
        let expected_bridge = naming::bridge_name("vpc-200");
        assert_eq!(
            attach_calls[0],
            format!("attach_to_bridge({expected_vxlan}, {expected_bridge})")
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
        assert_eq!(
            delete_calls[0],
            format!("delete_vxlan({})", naming::vxlan_name("vpc-300"))
        );
    }
}
