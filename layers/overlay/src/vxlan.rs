//! VXLAN interface management.
//!
//! One VXLAN interface per VPC per node: `syfvx-{vpc_id}`.
//! Created on-demand when the first VM in a VPC lands on this node.

use std::net::Ipv6Addr;

use crate::backend::{BackendResult, NetworkBackend};

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
/// Idempotent: if the interface already exists, this is a no-op.
///
/// Steps:
/// 1. Check if `syfvx-{vpc_id}` already exists.
/// 2. Create with `nolearning` + `proxy` flags, bring up.
/// 3. Attach to `syfbr-{vpc_id}`.
pub fn ensure_vxlan(
    backend: &dyn NetworkBackend,
    vpc_id: &str,
    vni: u32,
    local_ip: Ipv6Addr,
) -> BackendResult<()> {
    let name = vxlan_name(vpc_id);
    let bridge = bridge_name(vpc_id);

    if backend.interface_exists(&name)? {
        tracing::debug!(vxlan = %name, "VXLAN interface already exists, skipping creation");
        return Ok(());
    }

    tracing::info!(vxlan = %name, vni, %local_ip, "creating VXLAN interface");
    backend.create_vxlan(&name, vni, local_ip, VXLAN_PORT)?;

    tracing::info!(vxlan = %name, bridge = %bridge, "attaching VXLAN to VPC bridge");
    backend.attach_to_bridge(&name, &bridge)?;

    Ok(())
}

/// Delete the VXLAN interface for a VPC.
///
/// Idempotent: if the interface does not exist, this is a no-op.
pub fn remove_vxlan(backend: &dyn NetworkBackend, vpc_id: &str) -> BackendResult<()> {
    let name = vxlan_name(vpc_id);

    if !backend.interface_exists(&name)? {
        tracing::debug!(vxlan = %name, "VXLAN interface does not exist, skipping deletion");
        return Ok(());
    }

    tracing::info!(vxlan = %name, "deleting VXLAN interface");
    backend.delete_vxlan(&name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockBackend, MockCall};

    #[test]
    fn create_vxlan() {
        let backend = MockBackend::new();
        let local_ip: Ipv6Addr = "fd00::1".parse().unwrap();

        ensure_vxlan(&backend, "vpc-100", 100, local_ip).unwrap();

        let calls = backend.calls();
        // First call: interface_exists check
        assert_eq!(
            calls[0],
            MockCall::InterfaceExists {
                name: "syfvx-vpc-100".to_string(),
            }
        );
        // Second call: create_vxlan with correct VNI and local IP
        assert_eq!(
            calls[1],
            MockCall::CreateVxlan {
                name: "syfvx-vpc-100".to_string(),
                vni: 100,
                local_ip,
                port: VXLAN_PORT,
            }
        );
        // Third call: attach to bridge
        assert_eq!(
            calls[2],
            MockCall::AttachToBridge {
                interface: "syfvx-vpc-100".to_string(),
                bridge: "syfbr-vpc-100".to_string(),
            }
        );
    }

    #[test]
    fn correct_vni() {
        let backend = MockBackend::new();
        let local_ip: Ipv6Addr = "fd00::1".parse().unwrap();

        // Create with VNI 42 and verify the VNI matches
        ensure_vxlan(&backend, "vpc-42", 42, local_ip).unwrap();

        let calls = backend.calls();
        let create_call = calls
            .iter()
            .find(|c| matches!(c, MockCall::CreateVxlan { .. }))
            .expect("expected a CreateVxlan call");
        match create_call {
            MockCall::CreateVxlan { vni, .. } => assert_eq!(*vni, 42),
            _ => unreachable!(),
        }
    }

    #[test]
    fn attach_to_bridge() {
        let backend = MockBackend::new();
        let local_ip: Ipv6Addr = "fd00::1".parse().unwrap();

        ensure_vxlan(&backend, "vpc-200", 200, local_ip).unwrap();

        let attach_calls: Vec<_> = backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, MockCall::AttachToBridge { .. }))
            .collect();
        assert_eq!(attach_calls.len(), 1);
        assert_eq!(
            attach_calls[0],
            MockCall::AttachToBridge {
                interface: "syfvx-vpc-200".to_string(),
                bridge: "syfbr-vpc-200".to_string(),
            }
        );
    }

    #[test]
    fn delete_vxlan() {
        let backend = MockBackend::new();
        let local_ip: Ipv6Addr = "fd00::1".parse().unwrap();

        // Create then delete
        ensure_vxlan(&backend, "vpc-300", 300, local_ip).unwrap();
        remove_vxlan(&backend, "vpc-300").unwrap();

        let delete_calls: Vec<_> = backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, MockCall::DeleteVxlan { .. }))
            .collect();
        assert_eq!(delete_calls.len(), 1);
        assert_eq!(
            delete_calls[0],
            MockCall::DeleteVxlan {
                name: "syfvx-vpc-300".to_string(),
            }
        );
    }

    #[test]
    fn idempotent_create() {
        let backend = MockBackend::new();
        let local_ip: Ipv6Addr = "fd00::1".parse().unwrap();

        // First call creates the interface
        ensure_vxlan(&backend, "vpc-400", 400, local_ip).unwrap();
        // Second call should be a no-op (interface already exists in mock)
        ensure_vxlan(&backend, "vpc-400", 400, local_ip).unwrap();

        let create_calls: Vec<_> = backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, MockCall::CreateVxlan { .. }))
            .collect();
        // Only one actual create, second was idempotent
        assert_eq!(create_calls.len(), 1);
    }
}
