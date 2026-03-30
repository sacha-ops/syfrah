//! nftables rule management for VPC isolation and peering.
//!
//! The `syfrah` nftables table uses a `forward` chain to control cross-bridge
//! traffic. By default all forwarding between VPC bridges is denied (VPC
//! isolation). Peering adds explicit ACCEPT rules for the two bridge
//! interfaces in both directions.

use crate::backend::{BackendError, NetworkBackend};

/// Allow forwarding between two peered VPC bridges.
///
/// Adds two symmetric rules so traffic can flow in both directions:
/// - `iif {bridge_a} oif {bridge_b} accept`
/// - `iif {bridge_b} oif {bridge_a} accept`
pub fn apply_peering_rules(
    backend: &dyn NetworkBackend,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<(), BackendError> {
    backend.apply_peering_rules(bridge_a, bridge_b)
}

/// Remove forwarding rules between two previously-peered VPC bridges.
///
/// After removal, the default deny policy blocks cross-bridge traffic again.
pub fn remove_peering_rules(
    backend: &dyn NetworkBackend,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<(), BackendError> {
    backend.remove_peering_rules(bridge_a, bridge_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockCall, MockNetworkBackend};

    #[test]
    fn peering_forward_rules() {
        let backend = MockNetworkBackend::new();

        apply_peering_rules(&backend, "syfbr-100", "syfbr-200").unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            MockCall::ApplyPeeringRules {
                bridge_a: "syfbr-100".to_string(),
                bridge_b: "syfbr-200".to_string(),
            }
        );
    }

    #[test]
    fn unpeered_blocked() {
        // Without calling apply_peering_rules, no FORWARD rules exist.
        // The default deny policy (VPC isolation) blocks cross-bridge traffic.
        let backend = MockNetworkBackend::new();

        // No peering rules applied — the mock records nothing.
        let peering_calls =
            backend.calls_matching(|c| matches!(c, MockCall::ApplyPeeringRules { .. }));
        assert!(
            peering_calls.is_empty(),
            "no peering rules should exist without explicit apply"
        );
    }

    #[test]
    fn rules_removed_on_unpeer() {
        let backend = MockNetworkBackend::new();

        // Peer two VPC bridges.
        apply_peering_rules(&backend, "syfbr-100", "syfbr-200").unwrap();

        // Unpeer — removes FORWARD rules.
        remove_peering_rules(&backend, "syfbr-100", "syfbr-200").unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            MockCall::RemovePeeringRules {
                bridge_a: "syfbr-100".to_string(),
                bridge_b: "syfbr-200".to_string(),
            }
        );

        // After removal, no active peering rules remain — the default deny
        // policy resumes blocking cross-bridge forwarding.
    }
}
