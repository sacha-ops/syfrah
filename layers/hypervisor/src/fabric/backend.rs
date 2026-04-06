//! Network backend trait — abstraction over WireGuard, direct, or mock networking.
//!
//! Every backend implements the same interface. The fabric layer
//! is agnostic to the transport — only the backend knows how
//! packets flow between nodes.

use std::net::Ipv6Addr;

use syfrah_core::error::SyfrahError;

/// Network backend mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// Encrypted WireGuard mesh (default for public internet).
    #[default]
    WireGuard,
    /// Direct routing on private network (no tunnel, no encryption).
    Direct,
    /// Mock backend for testing (no-op, always up).
    Mock,
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WireGuard => write!(f, "wireguard"),
            Self::Direct => write!(f, "direct"),
            Self::Mock => write!(f, "mock"),
        }
    }
}

impl std::str::FromStr for NetworkMode {
    type Err = SyfrahError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "wireguard" | "wg" => Ok(Self::WireGuard),
            "direct" | "private" => Ok(Self::Direct),
            "mock" | "test" => Ok(Self::Mock),
            _ => Err(SyfrahError::validation(format!(
                "unknown network mode '{s}'. Must be: wireguard, direct, mock"
            ))),
        }
    }
}


/// Peer info passed to the backend.
pub struct BackendPeer {
    pub public_key: String,
    pub endpoint: Option<String>,
    pub mesh_ipv6: Ipv6Addr,
    pub keepalive_secs: u16,
}

/// Network status returned by the backend.
#[derive(Debug, Clone, Default)]
pub struct NetworkStatus {
    pub interface_up: bool,
    pub listen_port: u16,
    pub peer_count: usize,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// The contract every network backend must fulfill.
pub trait NetworkBackend: Send + Sync {
    /// Install any required system packages (e.g., wireguard-tools).
    fn ensure_installed(&self) -> Result<(), SyfrahError>;

    /// Setup the network interface with the node's identity.
    fn setup(
        &self,
        private_key: &str,
        listen_port: u16,
        mesh_ipv6: &Ipv6Addr,
        peers: &[BackendPeer],
    ) -> Result<(), SyfrahError>;

    /// Add a peer to the network.
    fn add_peer(&self, peer: &BackendPeer) -> Result<(), SyfrahError>;

    /// Remove a peer from the network.
    fn remove_peer(&self, public_key: &str, mesh_ipv6: &Ipv6Addr) -> Result<(), SyfrahError>;

    /// Update config with full peer list (reconcile).
    fn update_config(
        &self,
        private_key: &str,
        listen_port: u16,
        mesh_ipv6: &Ipv6Addr,
        peers: &[BackendPeer],
    ) -> Result<(), SyfrahError>;

    /// Check if the interface is up.
    fn is_up(&self) -> bool;

    /// Check if the service is active.
    fn is_active(&self) -> bool;

    /// Get network status.
    fn status(&self) -> Result<NetworkStatus, SyfrahError>;

    /// Start the network service.
    fn start(&self) -> Result<(), SyfrahError>;

    /// Stop the network service.
    fn stop(&self) -> Result<(), SyfrahError>;

    /// Teardown everything (uninstall service, remove config).
    fn teardown(&self) -> Result<(), SyfrahError>;

    /// The mode this backend implements.
    fn mode(&self) -> NetworkMode;
}

/// Create a backend from a mode string.
pub fn create_backend(mode: NetworkMode) -> Box<dyn NetworkBackend> {
    match mode {
        NetworkMode::WireGuard => Box::new(super::wireguard::WireGuardBackend),
        NetworkMode::Direct => Box::new(super::direct::DirectBackend),
        NetworkMode::Mock => Box::new(super::mock::MockBackend),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse() {
        assert_eq!("wireguard".parse::<NetworkMode>().unwrap(), NetworkMode::WireGuard);
        assert_eq!("wg".parse::<NetworkMode>().unwrap(), NetworkMode::WireGuard);
        assert_eq!("direct".parse::<NetworkMode>().unwrap(), NetworkMode::Direct);
        assert_eq!("mock".parse::<NetworkMode>().unwrap(), NetworkMode::Mock);
        assert!("invalid".parse::<NetworkMode>().is_err());
    }

    #[test]
    fn mode_display() {
        assert_eq!(NetworkMode::WireGuard.to_string(), "wireguard");
        assert_eq!(NetworkMode::Direct.to_string(), "direct");
        assert_eq!(NetworkMode::Mock.to_string(), "mock");
    }

    #[test]
    fn mode_default() {
        assert_eq!(NetworkMode::default(), NetworkMode::WireGuard);
    }

    #[test]
    fn create_backend_wireguard() {
        let b = create_backend(NetworkMode::WireGuard);
        assert_eq!(b.mode(), NetworkMode::WireGuard);
    }

    #[test]
    fn create_backend_mock() {
        let b = create_backend(NetworkMode::Mock);
        assert_eq!(b.mode(), NetworkMode::Mock);
        assert!(b.is_up()); // mock is always up
    }
}
