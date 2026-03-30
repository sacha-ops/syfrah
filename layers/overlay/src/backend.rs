use std::fmt;
use std::sync::{Arc, Mutex};

use ipnet::Ipv4Net;

/// Errors from network backend operations.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("command failed: {0}")]
    CommandFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BackendError>;

/// Abstraction over Linux networking primitives for testability.
///
/// All operations are idempotent: applying the same rule twice is a no-op,
/// removing a non-existent rule succeeds silently.
pub trait NetworkBackend: Send + Sync {
    /// Apply SNAT masquerade for a subnet behind a bridge.
    ///
    /// Creates the `syfrah_nat` table and postrouting chain if they don't exist,
    /// then adds a masquerade rule for traffic from `subnet` exiting via any
    /// interface other than `bridge`.
    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()>;

    /// Remove SNAT masquerade for a subnet behind a bridge.
    fn remove_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()>;
}

// ---------------------------------------------------------------------------
// LinuxBackend — real nftables commands
// ---------------------------------------------------------------------------

/// Production backend that executes real nftables commands.
pub struct LinuxBackend;

impl NetworkBackend for LinuxBackend {
    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()> {
        crate::nft::apply_nat(bridge, subnet)
    }

    fn remove_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()> {
        crate::nft::remove_nat(bridge, subnet)
    }
}

// ---------------------------------------------------------------------------
// MockBackend — records calls for unit tests
// ---------------------------------------------------------------------------

/// A recorded NAT operation for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatCall {
    ApplyNat { bridge: String, subnet: Ipv4Net },
    RemoveNat { bridge: String, subnet: Ipv4Net },
}

impl fmt::Display for NatCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NatCall::ApplyNat { bridge, subnet } => {
                write!(f, "apply_nat({bridge}, {subnet})")
            }
            NatCall::RemoveNat { bridge, subnet } => {
                write!(f, "remove_nat({bridge}, {subnet})")
            }
        }
    }
}

/// Test-only backend that records every call without touching the system.
#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    calls: Arc<Mutex<Vec<NatCall>>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<NatCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Clear recorded calls.
    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
}

impl NetworkBackend for MockBackend {
    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()> {
        self.calls.lock().unwrap().push(NatCall::ApplyNat {
            bridge: bridge.to_string(),
            subnet,
        });
        Ok(())
    }

    fn remove_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()> {
        self.calls.lock().unwrap().push(NatCall::RemoveNat {
            bridge: bridge.to_string(),
            subnet,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snat_rule_generated() {
        let mock = MockBackend::new();
        let subnet: Ipv4Net = "10.1.1.0/24".parse().unwrap();

        mock.apply_nat("syfbr-100", subnet).unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            NatCall::ApplyNat { bridge, subnet: s } => {
                assert_eq!(bridge, "syfbr-100");
                assert_eq!(s.to_string(), "10.1.1.0/24");
            }
            other => panic!("expected ApplyNat, got {other}"),
        }
    }

    #[test]
    fn masquerade_per_bridge() {
        let mock = MockBackend::new();
        let subnet_a: Ipv4Net = "10.1.1.0/24".parse().unwrap();
        let subnet_b: Ipv4Net = "10.2.1.0/24".parse().unwrap();

        mock.apply_nat("syfbr-100", subnet_a).unwrap();
        mock.apply_nat("syfbr-200", subnet_b).unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 2);

        // Each bridge/subnet pair gets its own rule.
        assert_eq!(
            calls[0],
            NatCall::ApplyNat {
                bridge: "syfbr-100".to_string(),
                subnet: subnet_a,
            }
        );
        assert_eq!(
            calls[1],
            NatCall::ApplyNat {
                bridge: "syfbr-200".to_string(),
                subnet: subnet_b,
            }
        );
    }
}
