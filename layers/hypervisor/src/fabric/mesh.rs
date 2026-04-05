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
    /// Unique hypervisor ID.
    pub id: syfrah_core::id::HypervisorId,
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
/// Validates the mesh name before creation.
pub fn create_mesh(
    name: &str,
) -> Result<(MeshIdentity, MeshSecret), syfrah_core::error::SyfrahError> {
    syfrah_core::validate::name(name)?;

    let secret = MeshSecret::generate();
    let prefix = addressing::generate_mesh_prefix();
    let id = MeshId::generate();

    let mesh = MeshIdentity {
        id,
        name: name.to_string(),
        prefix,
    };

    Ok((mesh, secret))
}

/// Create a new node identity (called by both init and join).
/// Validates name, region, zone, and port.
pub fn create_node(
    name: &str,
    region: &str,
    zone: &str,
    port: u16,
    endpoint: Option<String>,
    mesh_prefix: &Ipv6Addr,
) -> Result<NodeIdentity, syfrah_core::error::SyfrahError> {
    syfrah_core::validate::name(name)?;
    syfrah_core::validate::region(region)?;
    syfrah_core::validate::zone(zone)?;
    syfrah_core::validate::port(port)?;

    let (wg_private, wg_public) = crypto::generate_wg_keypair();

    let pub_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &wg_public)
        .unwrap_or_default();

    let mesh_ipv6 = addressing::derive_node_address(mesh_prefix, &pub_bytes);

    Ok(NodeIdentity {
        id: syfrah_core::id::HypervisorId::generate(),
        name: name.to_string(),
        region: region.to_string(),
        zone: zone.to_string(),
        wg_private_key: wg_private,
        wg_public_key: wg_public,
        wg_port: port,
        endpoint,
        mesh_ipv6,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_mesh_generates_valid_identity() {
        let (mesh, secret) = create_mesh("my-cloud").unwrap();
        assert_eq!(mesh.name, "my-cloud");
        assert!(mesh.id.as_str().starts_with("mesh-"));
        assert!(secret.to_string().starts_with("syf_sk_"));
        let first = mesh.prefix.segments()[0];
        assert!((0xfd00..=0xfdff).contains(&first));
    }

    #[test]
    fn create_mesh_unique() {
        let (a, _) = create_mesh("mesh-aaa").unwrap();
        let (b, _) = create_mesh("mesh-bbb").unwrap();
        assert_ne!(a.id.as_str(), b.id.as_str());
    }

    #[test]
    fn create_node_has_valid_identity() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let node = create_node("node-1", "eu", "fsn1", 51820, None, &mesh.prefix).unwrap();

        assert_eq!(node.name, "node-1");
        assert_eq!(node.region, "eu");
        assert_eq!(node.zone, "fsn1");
        assert_eq!(node.wg_port, 51820);
        assert!(!node.wg_private_key.is_empty());
        assert!(!node.wg_public_key.is_empty());
        assert!(addressing::is_in_prefix(&node.mesh_ipv6, &mesh.prefix));
    }

    #[test]
    fn create_node_unique_keys() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let a = create_node("node-aaa", "eu", "fsn1", 51820, None, &mesh.prefix).unwrap();
        let b = create_node("node-bbb", "eu", "fsn1", 51820, None, &mesh.prefix).unwrap();
        assert_ne!(a.wg_public_key, b.wg_public_key);
        assert_ne!(a.mesh_ipv6, b.mesh_ipv6);
    }

    #[test]
    fn create_node_with_endpoint() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let node = create_node(
            "node-1",
            "eu",
            "fsn1",
            51820,
            Some("46.224.166.60:51820".into()),
            &mesh.prefix,
        )
        .unwrap();
        assert_eq!(node.endpoint, Some("46.224.166.60:51820".into()));
    }

    #[test]
    fn node_identity_serde_roundtrip() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let node = create_node("node-1", "eu", "fsn1", 51820, None, &mesh.prefix).unwrap();
        let json = serde_json::to_string(&node).unwrap();
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "node-1");
        assert!(json.contains(&node.wg_public_key));
    }

    // ── #1: Validation tests ──

    #[test]
    fn create_mesh_rejects_empty_name() {
        assert!(create_mesh("").is_err());
    }

    #[test]
    fn create_mesh_rejects_short_name() {
        assert!(create_mesh("ab").is_err());
    }

    #[test]
    fn create_mesh_rejects_uppercase() {
        assert!(create_mesh("MyCloud").is_err());
    }

    #[test]
    fn create_node_rejects_empty_name() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        assert!(create_node("", "eu", "fsn1", 51820, None, &mesh.prefix).is_err());
    }

    #[test]
    fn create_node_rejects_bad_region() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        assert!(create_node("node-1", "EU!", "fsn1", 51820, None, &mesh.prefix).is_err());
    }

    #[test]
    fn create_node_rejects_bad_zone() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        assert!(create_node("node-1", "eu", "FSN 1", 51820, None, &mesh.prefix).is_err());
    }

    #[test]
    fn create_node_rejects_port_zero() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        assert!(create_node("node-1", "eu", "fsn1", 0, None, &mesh.prefix).is_err());
    }

    // ── #5: Private key persistence ──

    #[test]
    fn private_key_survives_serde() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let node = create_node("node-1", "eu", "fsn1", 51820, None, &mesh.prefix).unwrap();
        let original_private = node.wg_private_key.clone();
        assert!(!original_private.is_empty());

        let json = serde_json::to_string(&node).unwrap();
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.wg_private_key, original_private);
    }

    #[test]
    fn private_key_default_when_missing() {
        // Simulate receiving peer info without private key
        let json = r#"{"id":"hv-test","name":"n1","region":"eu","zone":"fsn1","wg_public_key":"abc","wg_port":51820,"mesh_ipv6":"fd01::1"}"#;
        let node: NodeIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(node.wg_private_key, ""); // defaults to empty
        assert_eq!(node.name, "n1");
    }

    // ── #2: Limits ──

    #[test]
    fn create_node_long_name() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let long_name = "a".repeat(63); // max allowed
        assert!(create_node(&long_name, "eu", "fsn1", 51820, None, &mesh.prefix).is_ok());

        let too_long = "a".repeat(64);
        assert!(create_node(&too_long, "eu", "fsn1", 51820, None, &mesh.prefix).is_err());
    }

    #[test]
    fn mesh_identity_serde() {
        let (mesh, _) = create_mesh("test-mesh").unwrap();
        let json = serde_json::to_string(&mesh).unwrap();
        let back: MeshIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "test-mesh");
        assert_eq!(back.prefix, mesh.prefix);
    }
}
