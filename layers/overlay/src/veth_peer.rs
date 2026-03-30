//! Veth peering between VPC bridges on the same node.
//!
//! Creates a veth pair that connects two VPC bridges, enabling cross-VPC
//! communication for peered VPCs.
//!
//! Naming convention: `syfpeer-{peering_id}-a` and `syfpeer-{peering_id}-b`.

use crate::backend::NetworkBackend;
use crate::error::Result;

/// Create a veth peer connection between two VPC bridges.
///
/// Steps:
/// 1. Create veth pair `syfpeer-{peering_id}-a` / `syfpeer-{peering_id}-b`
/// 2. Attach `-a` to `bridge_a`, `-b` to `bridge_b`
/// 3. Apply nftables peering rules to allow forwarding between bridges
pub async fn create_veth_peer(
    backend: &dyn NetworkBackend,
    peering_id: &str,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<()> {
    let name_a = format!("syfpeer-{peering_id}-a");
    let name_b = format!("syfpeer-{peering_id}-b");

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
    let name_a = format!("syfpeer-{peering_id}-a");

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

        create_veth_peer(&backend, "peer1", "syfbr-100", "syfbr-200")
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[0],
            "create_veth_pair(syfpeer-peer1-a, syfpeer-peer1-b)"
        );
        assert_eq!(calls[1], "attach_to_bridge(syfpeer-peer1-a, syfbr-100)");
        assert_eq!(calls[2], "attach_to_bridge(syfpeer-peer1-b, syfbr-200)");
        assert_eq!(calls[3], "apply_peering_rules(syfbr-100, syfbr-200)");
    }

    #[tokio::test]
    async fn delete_peer_cleans_up() {
        let backend = MockBackend::new();

        create_veth_peer(&backend, "peer3", "syfbr-500", "syfbr-600")
            .await
            .unwrap();

        backend.reset();

        delete_veth_peer(&backend, "peer3", "syfbr-500", "syfbr-600")
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], "remove_peering_rules(syfbr-500, syfbr-600)");
        assert_eq!(calls[1], "delete_tap(syfpeer-peer3-a)");
    }

    #[tokio::test]
    async fn peering_id_in_names() {
        let backend = MockBackend::new();

        create_veth_peer(&backend, "abc123", "syfbr-1", "syfbr-2")
            .await
            .unwrap();

        let calls = backend.calls();
        assert!(calls[0].contains("syfpeer-abc123-a"));
        assert!(calls[0].contains("syfpeer-abc123-b"));
    }
}
