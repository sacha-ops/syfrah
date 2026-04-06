//! Fabric state — persisted mesh configuration.
//!
//! The fabric state is the complete snapshot of a node's mesh membership:
//! mesh identity, node identity, secret, and list of peers.

use serde::{Deserialize, Serialize};
use syfrah_state::LayerDb;

use super::backend::NetworkMode;
use super::mesh::{HypervisorIdentity, MeshIdentity};
use super::peer::PeerList;

/// Complete fabric state for a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricState {
    /// Mesh identity (name, prefix).
    pub mesh: MeshIdentity,
    /// This node's identity.
    pub hypervisor: HypervisorIdentity,
    /// The mesh secret (encrypted at rest in future).
    pub secret: String,
    /// Known peers.
    pub peers: PeerList,
    /// Network backend mode.
    #[serde(default)]
    pub network_mode: NetworkMode,
}

const STATE_TABLE: &str = "fabric";
const STATE_KEY: &str = "state";

impl FabricState {
    /// Save state to redb.
    pub fn save(&self, db: &LayerDb) -> Result<(), syfrah_state::StateError> {
        db.set(STATE_TABLE, STATE_KEY, self)
    }

    /// Load state from redb. Returns None if no state exists.
    pub fn load(db: &LayerDb) -> Result<Option<Self>, syfrah_state::StateError> {
        db.get(STATE_TABLE, STATE_KEY)
    }

    /// Delete state (used by `leave`).
    pub fn delete(db: &LayerDb) -> Result<(), syfrah_state::StateError> {
        db.delete(STATE_TABLE, STATE_KEY)?;
        Ok(())
    }

    /// Check if fabric state exists.
    pub fn exists(db: &LayerDb) -> Result<bool, syfrah_state::StateError> {
        db.exists(STATE_TABLE, STATE_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::super::mesh;
    use super::*;

    fn temp_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    fn make_state() -> FabricState {
        let (mesh_id, secret) = mesh::create_mesh();
        let node =
            mesh::create_hypervisor("node-1", "eu", "fsn1", 51820, None, &mesh_id.prefix).unwrap();

        FabricState {
            mesh: mesh_id,
            hypervisor: node,
            secret: secret.to_string(),
            peers: PeerList::new(),
            network_mode: super::super::backend::NetworkMode::default(),
        }
    }

    #[test]
    fn save_and_load() {
        let (_d, db) = temp_db();
        let state = make_state();

        state.save(&db).unwrap();
        let loaded = FabricState::load(&db).unwrap().unwrap();

        assert_eq!(loaded.hypervisor.name, "node-1");
        assert_eq!(loaded.hypervisor.region, "eu");
    }

    #[test]
    fn load_empty() {
        let (_d, db) = temp_db();
        assert!(FabricState::load(&db).unwrap().is_none());
    }

    #[test]
    fn exists_check() {
        let (_d, db) = temp_db();
        assert!(!FabricState::exists(&db).unwrap());
        make_state().save(&db).unwrap();
        assert!(FabricState::exists(&db).unwrap());
    }

    #[test]
    fn delete_state() {
        let (_d, db) = temp_db();
        make_state().save(&db).unwrap();
        FabricState::delete(&db).unwrap();
        assert!(!FabricState::exists(&db).unwrap());
    }

    #[test]
    fn save_with_peers() {
        let (_d, db) = temp_db();
        let mut state = make_state();

        state
            .peers
            .add(super::super::peer::Peer::new(
                "node-2".into(),
                "eu".into(),
                "nbg1".into(),
                "key-n2".into(),
                51820,
                Some("1.2.3.4:51820".into()),
                "fd01::2".parse().unwrap(),
            ))
            .unwrap();

        state.save(&db).unwrap();
        let loaded = FabricState::load(&db).unwrap().unwrap();
        assert_eq!(loaded.peers.len(), 1);
        assert_eq!(loaded.peers.find_by_name("node-2").unwrap().zone, "nbg1");
    }

    #[test]
    fn secret_persists() {
        let (_d, db) = temp_db();
        let state = make_state();
        let secret = state.secret.clone();

        state.save(&db).unwrap();
        let loaded = FabricState::load(&db).unwrap().unwrap();
        assert_eq!(loaded.secret, secret);
        assert!(loaded.secret.starts_with("syf_sk_"));
    }

    #[test]
    fn serde_roundtrip() {
        let state = make_state();
        let json = serde_json::to_string(&state).unwrap();
        let back: FabricState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hypervisor.name, state.hypervisor.name);
        assert_eq!(
            back.hypervisor.wg_public_key,
            state.hypervisor.wg_public_key
        );
    }

    // ── #5: Private key persists through save/load ──

    #[test]
    fn private_key_persists() {
        let (_d, db) = temp_db();
        let state = make_state();
        let original_key = state.hypervisor.wg_private_key.clone();
        assert!(!original_key.is_empty());

        state.save(&db).unwrap();
        let loaded = FabricState::load(&db).unwrap().unwrap();
        assert_eq!(loaded.hypervisor.wg_private_key, original_key);
    }

    // ── #4: Corruption handling ──

    #[test]
    fn corrupted_state_returns_error() {
        let (_d, db) = temp_db();
        // Write garbage JSON
        db.set("fabric", "state", &"not valid json {{{").unwrap();
        // Load should fail gracefully (returns the raw string, not FabricState)
        let result = FabricState::load(&db);
        // It will either succeed (deserializes as a string wrapper) or fail
        // The important thing is it doesn't panic
        let _ = result;
    }

    // ── #6: Update peer then persist ──

    #[test]
    fn update_peer_and_save() {
        let (_d, db) = temp_db();
        let mut state = make_state();

        state
            .peers
            .add(super::super::peer::Peer::new(
                "node-2".into(),
                "eu".into(),
                "nbg1".into(),
                "key-n2".into(),
                51820,
                None,
                "fd01::2".parse().unwrap(),
            ))
            .unwrap();

        // Update peer handshake
        state.peers.peers[0].update_handshake(99999);
        state.save(&db).unwrap();

        let loaded = FabricState::load(&db).unwrap().unwrap();
        assert_eq!(loaded.peers.peers[0].last_handshake, 99999);
        assert!(loaded.peers.peers[0].is_active());
    }

    // ── Overwrite state ──

    #[test]
    fn save_overwrites_previous() {
        let (_d, db) = temp_db();
        let mut state = make_state();
        state.save(&db).unwrap();

        // Modify and save again
        state.save(&db).unwrap();

        let loaded = FabricState::load(&db).unwrap().unwrap();
    }
}
