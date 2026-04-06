//! Peer management — tracking remote nodes in the mesh.

use std::net::Ipv6Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use syfrah_core::id::NodeId;

fn default_wg_port() -> u16 {
    51820
}

/// A remote peer in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    /// Unique peer ID.
    pub id: NodeId,
    /// Human-readable name.
    pub name: String,
    /// Region.
    pub region: String,
    /// Zone.
    pub zone: String,
    /// WireGuard public key (base64).
    pub wg_public_key: String,
    /// WireGuard listen port.
    #[serde(default = "default_wg_port")]
    pub wg_port: u16,
    /// WireGuard endpoint (IP:port).
    pub endpoint: Option<String>,
    /// Peer's mesh IPv6 address.
    pub mesh_ipv6: Ipv6Addr,
    /// Current status.
    pub status: PeerStatus,
    /// Last successful handshake (unix epoch seconds).
    pub last_handshake: u64,
    /// When this peer was added.
    pub added_at: u64,
}

/// Peer connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerStatus {
    Active,
    Unreachable,
    Removed,
}

impl Peer {
    /// Create a new peer from join information.
    pub fn new(
        name: String,
        region: String,
        zone: String,
        wg_public_key: String,
        wg_port: u16,
        endpoint: Option<String>,
        mesh_ipv6: Ipv6Addr,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id: NodeId::generate(),
            name,
            region,
            zone,
            wg_public_key,
            wg_port,
            endpoint,
            mesh_ipv6,
            status: PeerStatus::Active,
            last_handshake: 0,
            added_at: now,
        }
    }

    /// Update the handshake timestamp.
    pub fn update_handshake(&mut self, timestamp: u64) {
        self.last_handshake = timestamp;
        self.status = PeerStatus::Active;
    }

    /// Mark as unreachable.
    pub fn mark_unreachable(&mut self) {
        self.status = PeerStatus::Unreachable;
    }

    /// Is this peer currently reachable?
    pub fn is_active(&self) -> bool {
        self.status == PeerStatus::Active
    }
}

/// A collection of peers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerList {
    pub peers: Vec<Peer>,
}

impl PeerList {
    pub fn new() -> Self {
        Self { peers: Vec::new() }
    }

    /// Add a peer. Returns error if public key or name already exists.
    pub fn add(&mut self, peer: Peer) -> Result<(), String> {
        if self
            .peers
            .iter()
            .any(|p| p.wg_public_key == peer.wg_public_key)
        {
            return Err(format!(
                "peer with key {} already exists",
                peer.wg_public_key
            ));
        }
        if self.peers.iter().any(|p| p.name == peer.name) {
            return Err(format!("peer with name {} already exists", peer.name));
        }
        self.peers.push(peer);
        Ok(())
    }

    /// Remove a peer by name or public key.
    pub fn remove(&mut self, name_or_key: &str) -> Option<Peer> {
        if let Some(pos) = self
            .peers
            .iter()
            .position(|p| p.name == name_or_key || p.wg_public_key == name_or_key)
        {
            Some(self.peers.remove(pos))
        } else {
            None
        }
    }

    /// Find a peer by name.
    pub fn find_by_name(&self, name: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.name == name)
    }

    /// Find a peer by public key.
    pub fn find_by_key(&self, key: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.wg_public_key == key)
    }

    /// Count active peers.
    pub fn active_count(&self) -> usize {
        self.peers.iter().filter(|p| p.is_active()).count()
    }

    /// Count unreachable peers.
    pub fn unreachable_count(&self) -> usize {
        self.peers
            .iter()
            .filter(|p| p.status == PeerStatus::Unreachable)
            .count()
    }

    /// Total peer count.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(name: &str) -> Peer {
        Peer::new(
            name.into(),
            "eu".into(),
            "fsn1".into(),
            format!("key-{name}"),
            51820,
            Some("1.2.3.4:51820".into()),
            "fd01::1".parse().unwrap(),
        )
    }

    #[test]
    fn peer_new() {
        let p = make_peer("node-1");
        assert_eq!(p.name, "node-1");
        assert!(p.is_active());
        assert!(p.id.as_str().starts_with("node-"));
    }

    #[test]
    fn peer_handshake() {
        let mut p = make_peer("n1");
        p.mark_unreachable();
        assert!(!p.is_active());
        p.update_handshake(12345);
        assert!(p.is_active());
        assert_eq!(p.last_handshake, 12345);
    }

    #[test]
    fn peer_list_add() {
        let mut list = PeerList::new();
        assert!(list.add(make_peer("n1")).is_ok());
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn peer_list_add_duplicate() {
        let mut list = PeerList::new();
        list.add(make_peer("n1")).unwrap();
        assert!(list.add(make_peer("n1")).is_err()); // same key
    }

    #[test]
    fn peer_list_remove_by_name() {
        let mut list = PeerList::new();
        list.add(make_peer("n1")).unwrap();
        let removed = list.remove("n1");
        assert!(removed.is_some());
        assert!(list.is_empty());
    }

    #[test]
    fn peer_list_remove_by_key() {
        let mut list = PeerList::new();
        list.add(make_peer("n1")).unwrap();
        let removed = list.remove("key-n1");
        assert!(removed.is_some());
    }

    #[test]
    fn peer_list_remove_nonexistent() {
        let mut list = PeerList::new();
        assert!(list.remove("nope").is_none());
    }

    #[test]
    fn peer_list_find() {
        let mut list = PeerList::new();
        list.add(make_peer("n1")).unwrap();
        assert!(list.find_by_name("n1").is_some());
        assert!(list.find_by_name("n2").is_none());
        assert!(list.find_by_key("key-n1").is_some());
    }

    #[test]
    fn peer_list_counts() {
        let mut list = PeerList::new();
        list.add(make_peer("n1")).unwrap();
        let mut p2 = make_peer("n2");
        // Need unique key
        p2.wg_public_key = "key-n2-unique".into();
        list.add(p2).unwrap();

        assert_eq!(list.active_count(), 2);
        assert_eq!(list.unreachable_count(), 0);

        list.peers[1].mark_unreachable();
        assert_eq!(list.active_count(), 1);
        assert_eq!(list.unreachable_count(), 1);
    }

    #[test]
    fn peer_serde_roundtrip() {
        let p = make_peer("n1");
        let json = serde_json::to_string(&p).unwrap();
        let back: Peer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "n1");
        assert_eq!(back.status, PeerStatus::Active);
    }
}
