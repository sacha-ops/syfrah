//! Raft type configuration for Syfrah.

use serde::{Deserialize, Serialize};

use crate::commands::{StateMachineCommand, StateMachineResponse};

/// Node identity in the Raft cluster.
///
/// The `id` is derived from a hash of the fabric node identity.
/// The `addr` is the fabric IPv6 address + Raft port (e.g. `[fd00::1]:7200`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct SyfrahNode {
    /// Fabric IPv6 address + port for Raft RPCs.
    pub addr: String,
}

impl std::fmt::Display for SyfrahNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyfrahNode({})", self.addr)
    }
}

openraft::declare_raft_types!(
    /// Raft type configuration for Syfrah control plane.
    pub SyfrahRaftConfig:
        D = StateMachineCommand,
        R = StateMachineResponse,
        Node = SyfrahNode,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_display() {
        let node = SyfrahNode {
            addr: "[fd00::1]:7200".to_string(),
        };
        assert_eq!(format!("{node}"), "SyfrahNode([fd00::1]:7200)");
    }

    #[test]
    fn node_serde_roundtrip() {
        let node = SyfrahNode {
            addr: "[fd00::1]:7200".to_string(),
        };
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: SyfrahNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, deserialized);
    }
}
