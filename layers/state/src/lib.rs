#![allow(clippy::result_large_err)]
//! State persistence for Syfrah.
//!
//! Two backends:
//! - **`LocalDb`** — JSON file store for bootstrap state (fabric identity, WG keys).
//!   Works before TiKV is up. Replaces the old redb-based `LayerDb`.
//! - **`ClusterDb`** — TiKV-backed distributed KV store for everything else
//!   (VMs, VPCs, users, subnets, etc.). Replicated across the mesh via Raft.
//!
//! # Usage
//!
//! ```no_run
//! use syfrah_state::LocalDb;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Peer { name: String }
//!
//! let db = LocalDb::open("fabric").unwrap();
//! db.set("state", "main", &Peer { name: "node-1".into() }).unwrap();
//! let peer: Option<Peer> = db.get("state", "main").unwrap();
//! ```

pub mod cluster;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use cluster::ClusterDb;

// ═══════════════════════════════════════════════════
// Errors
// ═══════════════════════════════════════════════════

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("database error: {0}")]
    Database(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StateError>;

// ═══════════════════════════════════════════════════
// LocalDb — JSON file store for bootstrap state
// ═══════════════════════════════════════════════════

fn syfrah_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
}

/// Store layout: `~/.syfrah/{layer}.json`
/// Internal format: `{ "table/key": <json_value>, ... }`
type StoreMap = HashMap<String, serde_json::Value>;

/// Local JSON file-backed store. Used for bootstrap state that must
/// be available before TiKV starts (mesh identity, WG keys, peers).
///
/// Thread-safe via `Arc<Mutex<...>>`. Clone is cheap.
#[derive(Clone, Debug)]
pub struct LocalDb {
    path: PathBuf,
    data: Arc<Mutex<StoreMap>>,
}

impl LocalDb {
    /// Open (or create) a local store for a layer.
    /// Stores at `~/.syfrah/{layer}.json`.
    pub fn open(layer: &str) -> Result<Self> {
        let dir = syfrah_dir();
        std::fs::create_dir_all(&dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }

        let path = dir.join(format!("{layer}.json"));
        Self::open_at(&path)
    }

    /// Open at a specific path.
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let data = if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            serde_json::from_str(&contents)
                .map_err(|e| StateError::Serialization(e.to_string()))?
        } else {
            StoreMap::new()
        };

        #[cfg(unix)]
        if path.exists() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(Self {
            path: path.to_path_buf(),
            data: Arc::new(Mutex::new(data)),
        })
    }

    /// Set a value (serialized to JSON).
    pub fn set<T: Serialize>(
        &self,
        table: &str,
        key: &str,
        value: &T,
    ) -> Result<()> {
        let json_value = serde_json::to_value(value)
            .map_err(|e| StateError::Serialization(e.to_string()))?;

        let compound_key = format!("{table}/{key}");

        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.insert(compound_key, json_value);
        self.flush(&data)
    }

    /// Get a value (deserialized from JSON).
    pub fn get<T: DeserializeOwned>(
        &self,
        table: &str,
        key: &str,
    ) -> Result<Option<T>> {
        let compound_key = format!("{table}/{key}");
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());

        match data.get(&compound_key) {
            Some(val) => {
                let parsed = serde_json::from_value(val.clone())
                    .map_err(|e| StateError::Serialization(e.to_string()))?;
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }

    /// Delete a key.
    pub fn delete(&self, table: &str, key: &str) -> Result<()> {
        let compound_key = format!("{table}/{key}");
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.remove(&compound_key);
        self.flush(&data)
    }

    /// Check if a key exists.
    pub fn exists(&self, table: &str, key: &str) -> Result<bool> {
        let compound_key = format!("{table}/{key}");
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        Ok(data.contains_key(&compound_key))
    }

    /// Flush to disk.
    fn flush(&self, data: &StoreMap) -> Result<()> {
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| StateError::Serialization(e.to_string()))?;
        std::fs::write(&self.path, json)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }
}

// Backward compat alias — hypervisor code uses LayerDb
pub type LayerDb = LocalDb;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestPeer {
        name: String,
        zone: String,
    }

    #[test]
    fn open_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        let db = LocalDb::open_at(&path).unwrap();
        db.set("peers", "n1", &TestPeer { name: "n1".into(), zone: "fsn1".into() }).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn set_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        let peer = TestPeer { name: "node-1".into(), zone: "fsn1".into() };
        db.set("peers", "n1", &peer).unwrap();

        let loaded: Option<TestPeer> = db.get("peers", "n1").unwrap();
        assert_eq!(loaded, Some(peer));
    }

    #[test]
    fn get_missing() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();
        let result: Option<TestPeer> = db.get("peers", "nope").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        db.set("peers", "n1", &"value").unwrap();
        assert!(db.exists("peers", "n1").unwrap());

        db.delete("peers", "n1").unwrap();
        assert!(!db.exists("peers", "n1").unwrap());
    }

    #[test]
    fn exists_check() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        assert!(!db.exists("peers", "n1").unwrap());
        db.set("peers", "n1", &"value").unwrap();
        assert!(db.exists("peers", "n1").unwrap());
    }

    #[test]
    fn persistence_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");

        {
            let db = LocalDb::open_at(&path).unwrap();
            db.set("state", "main", &TestPeer { name: "n1".into(), zone: "fsn1".into() }).unwrap();
        }

        {
            let db = LocalDb::open_at(&path).unwrap();
            let loaded: Option<TestPeer> = db.get("state", "main").unwrap();
            assert_eq!(loaded.unwrap().name, "n1");
        }
    }

    #[test]
    fn overwrite_value() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        db.set("k", "v", &"first").unwrap();
        db.set("k", "v", &"second").unwrap();

        let val: Option<String> = db.get("k", "v").unwrap();
        assert_eq!(val, Some("second".into()));
    }

    #[test]
    fn multiple_tables() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        db.set("peers", "n1", &"peer-data").unwrap();
        db.set("mesh", "id", &"mesh-data").unwrap();

        let p: Option<String> = db.get("peers", "n1").unwrap();
        let m: Option<String> = db.get("mesh", "id").unwrap();
        assert_eq!(p, Some("peer-data".into()));
        assert_eq!(m, Some("mesh-data".into()));
    }

    #[test]
    fn serde_roundtrip_complex() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        let data = serde_json::json!({
            "name": "node-1",
            "peers": ["n2", "n3"],
            "nested": { "key": "value" },
        });
        db.set("state", "main", &data).unwrap();

        let loaded: Option<serde_json::Value> = db.get("state", "main").unwrap();
        assert_eq!(loaded.unwrap()["name"], "node-1");
    }

    #[test]
    fn open_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        // Don't create the file — open should create it
        let db = LocalDb::open_at(&path).unwrap();
        assert!(!db.exists("any", "key").unwrap());
    }

    #[test]
    fn clone_is_cheap() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();
        db.set("k", "v", &"value").unwrap();

        let db2 = db.clone();
        let val: Option<String> = db2.get("k", "v").unwrap();
        assert_eq!(val, Some("value".into()));
    }
}
