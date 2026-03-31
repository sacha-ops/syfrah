//! Veth peering between VPC bridges on the same node.
//!
//! Creates a veth pair that connects two VPC bridges, enabling cross-VPC
//! communication for peered VPCs.

use crate::backend::NetworkBackend;
use crate::error::Result;
use crate::naming;

/// Create a veth peer connection between two VPC bridges.
///
/// Steps:
/// 1. Create veth pair for the peering
/// 2. Attach end A to `bridge_a`, end B to `bridge_b`
/// 3. Apply nftables peering rules to allow forwarding between bridges
pub async fn create_veth_peer(
    backend: &dyn NetworkBackend,
    peering_id: &str,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<()> {
    let name_a = naming::peer_name_a(peering_id);
    let name_b = naming::peer_name_b(peering_id);

    // 1. Create veth pair.
    backend.create_veth_pair(&name_a, &name_b).await?;

    // 2. Attach each end to its bridge.
    backend.attach_to_bridge(&name_a, bridge_a).await?;
    backend.attach_to_bridge(&name_b, bridge_b).await?;

    // 3. Allow forwarding between the two bridges.
    backend.apply_peering_rules(bridge_a, bridge_b).await?;

    Ok(())
}

/// Delete a veth peer connection.
///
/// Removes the peering rules and the veth pair itself.
pub async fn delete_veth_peer(
    backend: &dyn NetworkBackend,
    peering_id: &str,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<()> {
    let name_a = naming::peer_name_a(peering_id);

    // 1. Remove peering forwarding rules.
    backend.remove_peering_rules(bridge_a, bridge_b).await?;

    // 2. Delete the veth pair (kernel auto-removes both ends).
    // We use delete_tap as a generic interface deletion method.
    backend.delete_tap(&name_a).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[tokio::test]
    async fn create_veth_peer_creates_and_attaches_both_ends() {
        let backend = MockBackend::new();
        let br_a = naming::bridge_name("100");
        let br_b = naming::bridge_name("200");

        create_veth_peer(&backend, "peer1", &br_a, &br_b)
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 4);
        let pa = naming::peer_name_a("peer1");
        let pb = naming::peer_name_b("peer1");
        assert_eq!(calls[0], format!("create_veth_pair({pa}, {pb})"));
        assert_eq!(calls[1], format!("attach_to_bridge({pa}, {br_a})"));
        assert_eq!(calls[2], format!("attach_to_bridge({pb}, {br_b})"));
        assert_eq!(calls[3], format!("apply_peering_rules({br_a}, {br_b})"));
    }

    #[tokio::test]
    async fn delete_peer_cleans_up() {
        let backend = MockBackend::new();
        let br_a = naming::bridge_name("500");
        let br_b = naming::bridge_name("600");

        create_veth_peer(&backend, "peer3", &br_a, &br_b)
            .await
            .unwrap();

        backend.reset();

        delete_veth_peer(&backend, "peer3", &br_a, &br_b)
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], format!("remove_peering_rules({br_a}, {br_b})"));
        assert_eq!(
            calls[1],
            format!("delete_tap({})", naming::peer_name_a("peer3"))
        );
    }

    #[tokio::test]
    async fn peering_id_in_names() {
        let backend = MockBackend::new();
        let br_a = naming::bridge_name("1");
        let br_b = naming::bridge_name("2");

        create_veth_peer(&backend, "abc123", &br_a, &br_b)
            .await
            .unwrap();

        let calls = backend.calls();
        let pa = naming::peer_name_a("abc123");
        let pb = naming::peer_name_b("abc123");
        assert!(calls[0].contains(&pa));
        assert!(calls[0].contains(&pb));
    }
}
