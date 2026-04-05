//! Mesh identity and configuration.
//!
//! A mesh is defined by:
//! - A shared secret (syf_sk_...)
//! - A /48 ULA IPv6 prefix
//! - Each node gets a /128 address derived from the prefix + its WG key

use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};
use syfrah_core::addressing;
use syfrah_core::crypto::{self, MeshSecret};
use syfrah_core::id::MeshId;

/// Complete mesh identity — everything needed to describe a mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshIdentity {
    /// Unique mesh ID.
    pub id: MeshId,
    /// Human-readable mesh name.
    pub name: String,
    /// The /48 ULA prefix for this mesh.
    pub prefix: Ipv6Addr,
}

/// A node's identity within the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// Human-readable node name (usually hostname).
    pub name: String,
    /// Region label.
    pub region: String,
    /// Zone label.
    pub zone: String,
    /// WireGuard private key (base64).
    /// Persisted locally but never shared with peers.
    #[serde(default)]
    pub wg_private_key: String,
    /// WireGuard public key (base64).
    pub wg_public_key: String,
    /// WireGuard listen port.
    pub wg_port: u16,
    /// Public endpoint (IP:port) for other nodes to connect.
    pub endpoint: Option<String>,
    /// This node's mesh IPv6 address (/128).
    pub mesh_ipv6: Ipv6Addr,
}

/// Create a new mesh (called by `hypervisor init`).
pub fn create_mesh(name: &str) -> (MeshIdentity, MeshSecret) {
    let secret = MeshSecret::generate();
    let prefix = addressing::generate_mesh_prefix();
    let id = MeshId::generate();

    let mesh = MeshIdentity {
        id,
        name: name.to_string(),
        prefix,
    };

    (mesh, secret)
}

/// Create a new node identity (called by both init and join).
pub fn create_node(
    name: &str,
    region: &str,
    zone: &str,
    port: u16,
    endpoint: Option<String>,
    mesh_prefix: &Ipv6Addr,
) -> NodeIdentity {
    let (wg_private, wg_public) = crypto::generate_wg_keypair();

    // Decode public key for address derivation
    let pub_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &wg_public)
        .unwrap_or_default();

    let mesh_ipv6 = addressing::derive_node_address(mesh_prefix, &pub_bytes);

    NodeIdentity {
        name: name.to_string(),
        region: region.to_string(),
        zone: zone.to_string(),
        wg_private_key: wg_private,
        wg_public_key: wg_public,
        wg_port: port,
        endpoint,
        mesh_ipv6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_mesh_generates_valid_identity() {
        let (mesh, secret) = create_mesh("my-cloud");
        assert_eq!(mesh.name, "my-cloud");
        assert!(mesh.id.as_str().starts_with("mesh-"));
        assert!(secret.to_string().starts_with("syf_sk_"));
        // Prefix should be ULA
        let first = mesh.prefix.segments()[0];
        assert!((0xfd00..=0xfdff).contains(&first));
    }

    #[test]
    fn create_mesh_unique() {
        let (a, _) = create_mesh("a");
        let (b, _) = create_mesh("b");
        assert_ne!(a.id.as_str(), b.id.as_str());
    }

    #[test]
    fn create_node_has_valid_identity() {
        let (mesh, _) = create_mesh("test");
        let node = create_node("node-1", "eu", "fsn1", 51820, None, &mesh.prefix);

        assert_eq!(node.name, "node-1");
        assert_eq!(node.region, "eu");
        assert_eq!(node.zone, "fsn1");
        assert_eq!(node.wg_port, 51820);
        assert!(!node.wg_private_key.is_empty());
        assert!(!node.wg_public_key.is_empty());
        // IPv6 should be in the mesh prefix
        assert!(addressing::is_in_prefix(&node.mesh_ipv6, &mesh.prefix));
    }

    #[test]
    fn create_node_unique_keys() {
        let (mesh, _) = create_mesh("test");
        let a = create_node("a", "eu", "fsn1", 51820, None, &mesh.prefix);
        let b = create_node("b", "eu", "fsn1", 51820, None, &mesh.prefix);
        assert_ne!(a.wg_public_key, b.wg_public_key);
        assert_ne!(a.mesh_ipv6, b.mesh_ipv6);
    }

    #[test]
    fn create_node_with_endpoint() {
        let (mesh, _) = create_mesh("test");
        let node = create_node(
            "node-1",
            "eu",
            "fsn1",
            51820,
            Some("46.224.166.60:51820".into()),
            &mesh.prefix,
        );
        assert_eq!(node.endpoint, Some("46.224.166.60:51820".into()));
    }

    #[test]
    fn node_identity_serde_roundtrip() {
        let (mesh, _) = create_mesh("test");
        let node = create_node("n1", "eu", "fsn1", 51820, None, &mesh.prefix);
        let json = serde_json::to_string(&node).unwrap();
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "n1");
        // Public key should be present
        assert!(json.contains(&node.wg_public_key));
    }
}
