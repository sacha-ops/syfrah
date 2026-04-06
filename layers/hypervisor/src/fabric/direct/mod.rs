//! Direct network backend — private network, no tunnel.
//!
//! For deployments on private networks (VLANs, cloud VPCs)
//! where nodes can reach each other directly. No encryption
//! at the fabric level — the network is assumed trusted.

use std::net::Ipv6Addr;
use std::process::Command;

use syfrah_core::error::SyfrahError;

use super::backend::{BackendPeer, NetworkBackend, NetworkMode, NetworkStatus};

/// Direct backend — routing only, no tunneling.
pub struct DirectBackend;

impl NetworkBackend for DirectBackend {
    fn ensure_installed(&self) -> Result<(), SyfrahError> {
        // No extra packages needed for direct routing
        Ok(())
    }

    fn setup(
        &self,
        _private_key: &str,
        _listen_port: u16,
        mesh_ipv6: &Ipv6Addr,
        peers: &[BackendPeer],
    ) -> Result<(), SyfrahError> {
        // Assign the mesh IPv6 to the loopback (so we have the address)
        let _ = Command::new("ip")
            .args(["-6", "addr", "add", &format!("{mesh_ipv6}/128"), "dev", "lo"])
            .output();

        // Add routes to peers
        for peer in peers {
            self.add_peer(peer)?;
        }

        tracing::info!(%mesh_ipv6, "direct: network setup (no tunnel)");
        Ok(())
    }

    fn add_peer(&self, peer: &BackendPeer) -> Result<(), SyfrahError> {
        // In direct mode, we need an explicit route to the peer's mesh IPv6
        // via their endpoint IP. If no endpoint, skip (same subnet).
        if let Some(ref endpoint) = peer.endpoint {
            let gw = endpoint.split(':').next().unwrap_or(endpoint);
            let _ = Command::new("ip")
                .args(["-6", "route", "add", &format!("{}/128", peer.mesh_ipv6), "via", gw])
                .output();
        }
        Ok(())
    }

    fn remove_peer(&self, _public_key: &str, mesh_ipv6: &Ipv6Addr) -> Result<(), SyfrahError> {
        let _ = Command::new("ip")
            .args(["-6", "route", "del", &format!("{mesh_ipv6}/128")])
            .output();
        Ok(())
    }

    fn update_config(
        &self,
        _private_key: &str,
        _listen_port: u16,
        _mesh_ipv6: &Ipv6Addr,
        peers: &[BackendPeer],
    ) -> Result<(), SyfrahError> {
        for peer in peers {
            self.add_peer(peer)?;
        }
        Ok(())
    }

    fn is_up(&self) -> bool {
        true // Direct mode is always "up" — it's the real network
    }

    fn is_active(&self) -> bool {
        true
    }

    fn status(&self) -> Result<NetworkStatus, SyfrahError> {
        Ok(NetworkStatus {
            interface_up: true,
            listen_port: 0,
            peer_count: 0,
            rx_bytes: 0,
            tx_bytes: 0,
        })
    }

    fn start(&self) -> Result<(), SyfrahError> {
        Ok(())
    }

    fn stop(&self) -> Result<(), SyfrahError> {
        Ok(())
    }

    fn teardown(&self) -> Result<(), SyfrahError> {
        // Remove the mesh IPv6 from loopback
        // (we don't track which address we added, so best-effort)
        Ok(())
    }

    fn mode(&self) -> NetworkMode {
        NetworkMode::Direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_is_always_up() {
        let b = DirectBackend;
        assert!(b.is_up());
        assert_eq!(b.mode(), NetworkMode::Direct);
    }
}
