//! Veth peering between VPC bridges on the same node.
//!
//! Creates a veth pair that connects two VPC bridges, enabling cross-VPC
//! communication for peered VPCs. Routes are added so each bridge knows
//! how to reach the other VPC's CIDR.
//!
//! Naming convention: `syfpeer-{peering_id}-a` and `syfpeer-{peering_id}-b`.

use crate::backend::NetworkBackend;
use crate::error::Result;

/// Create a veth peer connection between two VPC bridges.
///
/// This is idempotent: if the veth pair already exists, it is a no-op.
///
/// Steps:
/// 1. Create veth pair `syfpeer-{peering_id}-a` / `syfpeer-{peering_id}-b`
/// 2. Attach `-a` to `bridge_a`, `-b` to `bridge_b`
/// 3. Bring both interfaces up
/// 4. Add route on `-a` for `vpc_b_cidr` and on `-b` for `vpc_a_cidr`
/// 5. Apply nftables peering rules to allow forwarding between bridges
pub async fn create_veth_peer(
    backend: &dyn NetworkBackend,
    peering_id: &str,
    bridge_a: &str,
    bridge_b: &str,
    vpc_a_cidr: &str,
    vpc_b_cidr: &str,
) -> Result<()> {
    let name_a = format!("syfpeer-{peering_id}-a");
    let name_b = format!("syfpeer-{peering_id}-b");

    // Idempotent: skip if already created.
    if backend.link_exists(&name_a).await? {
        return Ok(());
    }

    // 1. Create veth pair.
    backend.create_veth_pair(&name_a, &name_b).await?;

    // 2. Attach each end to its bridge.
    backend.attach_to_bridge(&name_a, bridge_a).await?;
    backend.attach_to_bridge(&name_b, bridge_b).await?;

    // 3. Bring both interfaces up.
    backend.set_link_up(&name_a).await?;
    backend.set_link_up(&name_b).await?;

    // 4. Add routes so each bridge can reach the other VPC's CIDR.
    backend.add_route(vpc_b_cidr, &name_a).await?;
    backend.add_route(vpc_a_cidr, &name_b).await?;

    // 5. Allow forwarding between the two bridges.
    backend.apply_peering_rules(bridge_a, bridge_b).await?;

    Ok(())
}

/// Delete a veth peer connection.
///
/// Removes the routes, peering rules, and the veth pair itself.
/// Idempotent: if the veth pair does not exist, it is a no-op.
pub async fn delete_veth_peer(
    backend: &dyn NetworkBackend,
    peering_id: &str,
    bridge_a: &str,
    bridge_b: &str,
    vpc_a_cidr: &str,
    vpc_b_cidr: &str,
) -> Result<()> {
    let name_a = format!("syfpeer-{peering_id}-a");
    let name_b = format!("syfpeer-{peering_id}-b");

    // Idempotent: skip if already removed.
    if !backend.link_exists(&name_a).await? {
        return Ok(());
    }

    // 1. Remove routes first (they reference the interfaces).
    backend.delete_route(vpc_b_cidr, &name_a).await?;
    backend.delete_route(vpc_a_cidr, &name_b).await?;

    // 2. Remove peering forwarding rules.
    backend.remove_peering_rules(bridge_a, bridge_b).await?;

    // 3. Delete the veth pair (kernel auto-removes both ends).
    backend.delete_veth_pair(&name_a).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[tokio::test]
    async fn create_veth_peer_creates_and_attaches_both_ends() {
        let backend = MockBackend::new();

        create_veth_peer(
            &backend,
            "peer1",
            "syfbr-100",
            "syfbr-200",
            "10.1.0.0/16",
            "10.2.0.0/16",
        )
        .await
        .unwrap();

        let calls = backend.calls();
        // link_exists check + create_veth_pair + 2 attach + 2 set_link_up + 2 add_route + apply_peering_rules
        assert_eq!(calls.len(), 9);

        assert_eq!(calls[0], "link_exists(syfpeer-peer1-a)");
        assert_eq!(
            calls[1],
            "create_veth_pair(syfpeer-peer1-a, syfpeer-peer1-b)"
        );
        assert_eq!(calls[2], "attach_to_bridge(syfpeer-peer1-a, syfbr-100)");
        assert_eq!(calls[3], "attach_to_bridge(syfpeer-peer1-b, syfbr-200)");
        assert_eq!(calls[4], "set_link_up(syfpeer-peer1-a)");
        assert_eq!(calls[5], "set_link_up(syfpeer-peer1-b)");
        assert_eq!(calls[6], "add_route(10.2.0.0/16, syfpeer-peer1-a)");
        assert_eq!(calls[7], "add_route(10.1.0.0/16, syfpeer-peer1-b)");
        assert_eq!(calls[8], "apply_peering_rules(syfbr-100, syfbr-200)");
    }

    #[tokio::test]
    async fn create_veth_peer_is_idempotent() {
        let backend = MockBackend::new();
        // Pre-register the link so link_exists returns true.
        backend.add_existing_link("syfpeer-peer1-a");

        create_veth_peer(
            &backend,
            "peer1",
            "syfbr-100",
            "syfbr-200",
            "10.1.0.0/16",
            "10.2.0.0/16",
        )
        .await
        .unwrap();

        let calls = backend.calls();
        // Only the link_exists check; no creation.
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "link_exists(syfpeer-peer1-a)");
    }

    #[tokio::test]
    async fn add_routes_for_peer_cidrs() {
        let backend = MockBackend::new();

        create_veth_peer(
            &backend,
            "peer2",
            "syfbr-300",
            "syfbr-400",
            "10.3.0.0/16",
            "10.4.0.0/16",
        )
        .await
        .unwrap();

        let calls = backend.calls();
        // Verify route calls: vpc_b_cidr via -a, vpc_a_cidr via -b
        let route_calls: Vec<&String> = calls
            .iter()
            .filter(|c| c.starts_with("add_route"))
            .collect();
        assert_eq!(route_calls.len(), 2);
        assert_eq!(route_calls[0], "add_route(10.4.0.0/16, syfpeer-peer2-a)");
        assert_eq!(route_calls[1], "add_route(10.3.0.0/16, syfpeer-peer2-b)");
    }

    #[tokio::test]
    async fn delete_peer_cleans_up() {
        let backend = MockBackend::new();

        // First create, then delete.
        create_veth_peer(
            &backend,
            "peer3",
            "syfbr-500",
            "syfbr-600",
            "10.5.0.0/16",
            "10.6.0.0/16",
        )
        .await
        .unwrap();

        backend.reset();

        delete_veth_peer(
            &backend,
            "peer3",
            "syfbr-500",
            "syfbr-600",
            "10.5.0.0/16",
            "10.6.0.0/16",
        )
        .await
        .unwrap();

        let calls = backend.calls();
        // link_exists + 2 delete_route + remove_peering_rules + delete_veth_pair
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[0], "link_exists(syfpeer-peer3-a)");
        assert_eq!(calls[1], "delete_route(10.6.0.0/16, syfpeer-peer3-a)");
        assert_eq!(calls[2], "delete_route(10.5.0.0/16, syfpeer-peer3-b)");
        assert_eq!(calls[3], "remove_peering_rules(syfbr-500, syfbr-600)");
        assert_eq!(calls[4], "delete_veth_pair(syfpeer-peer3-a)");
    }

    #[tokio::test]
    async fn delete_peer_is_idempotent() {
        let backend = MockBackend::new();
        // Link does not exist — delete should be a no-op.
        delete_veth_peer(
            &backend,
            "peer4",
            "syfbr-700",
            "syfbr-800",
            "10.7.0.0/16",
            "10.8.0.0/16",
        )
        .await
        .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "link_exists(syfpeer-peer4-a)");
    }

    #[tokio::test]
    async fn cleanup_routes_removed_on_delete() {
        let backend = MockBackend::new();

        // Create the peering first.
        create_veth_peer(
            &backend,
            "peer5",
            "syfbr-900",
            "syfbr-1000",
            "10.9.0.0/16",
            "10.10.0.0/16",
        )
        .await
        .unwrap();

        backend.reset();

        delete_veth_peer(
            &backend,
            "peer5",
            "syfbr-900",
            "syfbr-1000",
            "10.9.0.0/16",
            "10.10.0.0/16",
        )
        .await
        .unwrap();

        let calls = backend.calls();
        let route_del_calls: Vec<&String> = calls
            .iter()
            .filter(|c| c.starts_with("delete_route"))
            .collect();
        assert_eq!(route_del_calls.len(), 2);
        assert_eq!(
            route_del_calls[0],
            "delete_route(10.10.0.0/16, syfpeer-peer5-a)"
        );
        assert_eq!(
            route_del_calls[1],
            "delete_route(10.9.0.0/16, syfpeer-peer5-b)"
        );
    }
}
