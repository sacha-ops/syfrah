#![allow(clippy::result_large_err)]
//! Embedded state persistence for Syfrah layers.
//!
//! Each layer gets its own redb database file at `~/.syfrah/{layer}.redb`.
//! Provides a thin, typed wrapper around redb with:
//! - One file per layer (isolation)
//! - JSON serialization for values
//! - Typed get/set/delete/list/exists/count
//! - Atomic batches (multi-table ACID transactions)
//! - Metrics (u64 counters)
//! - Arc-safe for async sharing
//!
//! # Usage
//!
//! ```no_run
//! use syfrah_state::LayerDb;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Peer { name: String }
//!
//! let db = LayerDb::open("fabric").unwrap();
//! db.set("peers", "node-1", &Peer { name: "node-1".into() }).unwrap();
//! let peer: Option<Peer> = db.get("peers", "node-1").unwrap();
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Errors from the state store.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("database error: {0}")]
    Db(#[from] redb::DatabaseError),
    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StateError>;

/// Base directory for all syfrah state files.
fn syfrah_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
}

/// Get the redb file path for a given layer.
pub fn db_path(layer: &str) -> PathBuf {
    syfrah_dir().join(format!("{layer}.redb"))
}

/// A per-layer state database backed by redb.
///
/// Thread-safe (via `Arc<Database>`) and safe to share across tokio tasks.
#[derive(Clone, Debug)]
pub struct LayerDb {
    db: Arc<Database>,
    layer: String,
}

impl LayerDb {
    /// Open (or create) the redb database for a layer.
    pub fn open(layer: &str) -> Result<Self> {
        let dir = syfrah_dir();
        std::fs::create_dir_all(&dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }

        let path = db_path(layer);
        let db = Database::create(&path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(Self {
            db: Arc::new(db),
            layer: layer.to_string(),
        })
    }

    /// Open with a custom path (for testing).
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let db = Database::create(path)?;
        let layer = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("test")
            .to_string();
        Ok(Self {
            db: Arc::new(db),
            layer,
        })
    }

    /// Layer name.
    pub fn layer(&self) -> &str {
        &self.layer
    }

    /// Get a value by key. Returns `None` if key or table doesn't exist.
    pub fn get<T: DeserializeOwned>(&self, table: &str, key: &str) -> Result<Option<T>> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(StateError::Table(e)),
        };

        match t.get(key).map_err(StateError::Storage)? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Set a value. Creates the table if it doesn't exist.
    pub fn set<T: Serialize>(&self, table: &str, key: &str, value: &T) -> Result<()> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let bytes = serde_json::to_vec(value)?;

        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(td)?;
            t.insert(key, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Delete a key. Returns true if it existed.
    pub fn delete(&self, table: &str, key: &str) -> Result<bool> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);

        let txn = self.db.begin_write()?;
        let existed = {
            let mut t = txn.open_table(td)?;
            let removed = t.remove(key)?;
            let ex = removed.is_some();
            drop(removed);
            ex
        };
        txn.commit()?;
        Ok(existed)
    }

    /// List all key-value pairs in a table.
    pub fn list<T: DeserializeOwned>(&self, table: &str) -> Result<Vec<(String, T)>> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(StateError::Table(e)),
        };

        let mut results = Vec::new();
        for entry in t.iter().map_err(StateError::Storage)? {
            let (k, v) = entry.map_err(StateError::Storage)?;
            results.push((k.value().to_string(), serde_json::from_slice(v.value())?));
        }
        Ok(results)
    }

    /// Check if a key exists.
    pub fn exists(&self, table: &str, key: &str) -> Result<bool> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(false),
            Err(e) => return Err(StateError::Table(e)),
        };

        Ok(t.get(key).map_err(StateError::Storage)?.is_some())
    }

    /// Count entries in a table.
    pub fn count(&self, table: &str) -> Result<u64> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        match txn.open_table(td) {
            Ok(t) => t.len().map_err(StateError::Storage),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
            Err(redb::TableError::TableTypeMismatch { .. }) => {
                // Try u64 value type (metrics table)
                let td_u64: TableDefinition<&str, u64> = TableDefinition::new(table);
                let txn = self.db.begin_read()?;
                match txn.open_table(td_u64) {
                    Ok(t) => t.len().map_err(StateError::Storage),
                    Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
                    Err(e) => Err(StateError::Table(e)),
                }
            }
            Err(e) => Err(StateError::Table(e)),
        }
    }

    /// Get a u64 metric. Returns 0 if not set.
    pub fn get_metric(&self, key: &str) -> Result<u64> {
        let td: TableDefinition<&str, u64> = TableDefinition::new("metrics");
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(e) => return Err(StateError::Table(e)),
        };

        Ok(t.get(key)
            .map_err(StateError::Storage)?
            .map(|v| v.value())
            .unwrap_or(0))
    }

    /// Set a u64 metric.
    pub fn set_metric(&self, key: &str, value: u64) -> Result<()> {
        let td: TableDefinition<&str, u64> = TableDefinition::new("metrics");

        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(td)?;
            t.insert(key, value)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Increment a u64 metric. Returns the new value.
    pub fn inc_metric(&self, key: &str, delta: u64) -> Result<u64> {
        let td: TableDefinition<&str, u64> = TableDefinition::new("metrics");

        let txn = self.db.begin_write()?;
        let new_val = {
            let mut t = txn.open_table(td)?;
            let current = t.get(key)?.map(|v| v.value()).unwrap_or(0);
            let n = current + delta;
            t.insert(key, n)?;
            n
        };
        txn.commit()?;
        Ok(new_val)
    }

    /// Execute a batch of writes in a single ACID transaction.
    pub fn batch<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&BatchWriter) -> Result<()>,
    {
        let txn = self.db.begin_write()?;
        let writer = BatchWriter { txn: &txn };
        f(&writer)?;
        txn.commit()?;
        Ok(())
    }

    /// Atomically allocate the next counter value (read + increment in one txn).
    pub fn next_counter(&self, table: &str, key: &str, start: u32) -> Result<u32> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);

        let txn = self.db.begin_write()?;
        let current = {
            let t = txn.open_table(td)?;
            let access = t.get(key).map_err(StateError::Storage)?;
            let val = match &access {
                Some(v) => serde_json::from_slice::<u32>(v.value())?,
                None => start,
            };
            drop(access);
            val
        };
        {
            let mut t = txn.open_table(td)?;
            let bytes = serde_json::to_vec(&(current + 1))?;
            t.insert(key, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(current)
    }

    /// Export all entries as raw bytes (for snapshots).
    pub fn export_raw(&self, table: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(StateError::Table(e)),
        };

        let mut results = Vec::new();
        for entry in t.iter().map_err(StateError::Storage)? {
            let (k, v) = entry.map_err(StateError::Storage)?;
            results.push((k.value().to_string(), v.value().to_vec()));
        }
        Ok(results)
    }

    /// Import raw entries, replacing all existing data (for snapshot restore).
    pub fn import_raw(&self, table: &str, entries: &[(String, Vec<u8>)]) -> Result<()> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(td)?;
            // Clear existing
            let keys: Vec<String> = t
                .iter()
                .map_err(StateError::Storage)?
                .filter_map(|e| e.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for key in &keys {
                t.remove(key.as_str())?;
            }
            // Insert new
            for (k, v) in entries {
                t.insert(k.as_str(), v.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Delete the database file for a layer.
    pub fn destroy(layer: &str) -> Result<()> {
        let path = db_path(layer);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Check if a layer's database exists.
    pub fn layer_exists(layer: &str) -> bool {
        db_path(layer).exists()
    }
}

/// Writer within a batch transaction. All ops are committed atomically.
pub struct BatchWriter<'a> {
    txn: &'a redb::WriteTransaction,
}

impl BatchWriter<'_> {
    pub fn set<T: Serialize>(&self, table: &str, key: &str, value: &T) -> Result<()> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let bytes = serde_json::to_vec(value)?;
        let mut t = self.txn.open_table(td)?;
        t.insert(key, bytes.as_slice())?;
        Ok(())
    }

    pub fn delete(&self, table: &str, key: &str) -> Result<()> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let mut t = self.txn.open_table(td)?;
        t.remove(key)?;
        Ok(())
    }

    pub fn set_metric(&self, key: &str, value: u64) -> Result<()> {
        let td: TableDefinition<&str, u64> = TableDefinition::new("metrics");
        let mut t = self.txn.open_table(td)?;
        t.insert(key, value)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestPeer {
        name: String,
    }

    fn temp_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn set_and_get() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        db.set("peers", "k1", &p).unwrap();
        assert_eq!(db.get::<TestPeer>("peers", "k1").unwrap(), Some(p));
    }

    #[test]
    fn get_missing() {
        let (_d, db) = temp_db();
        assert_eq!(db.get::<TestPeer>("peers", "nope").unwrap(), None);
    }

    #[test]
    fn get_missing_table() {
        let (_d, db) = temp_db();
        assert_eq!(db.get::<TestPeer>("nope", "key").unwrap(), None);
    }

    #[test]
    fn delete_key() {
        let (_d, db) = temp_db();
        db.set("t", "k", &"v").unwrap();
        assert!(db.delete("t", "k").unwrap());
        assert!(!db.delete("t", "k").unwrap());
        assert_eq!(db.get::<String>("t", "k").unwrap(), None);
    }

    #[test]
    fn list_entries() {
        let (_d, db) = temp_db();
        db.set("t", "a", &"1").unwrap();
        db.set("t", "b", &"2").unwrap();
        assert_eq!(db.list::<String>("t").unwrap().len(), 2);
    }

    #[test]
    fn list_empty() {
        let (_d, db) = temp_db();
        assert_eq!(db.list::<String>("t").unwrap().len(), 0);
    }

    #[test]
    fn exists() {
        let (_d, db) = temp_db();
        assert!(!db.exists("t", "k").unwrap());
        db.set("t", "k", &"v").unwrap();
        assert!(db.exists("t", "k").unwrap());
    }

    #[test]
    fn count() {
        let (_d, db) = temp_db();
        assert_eq!(db.count("t").unwrap(), 0);
        db.set("t", "a", &"1").unwrap();
        db.set("t", "b", &"2").unwrap();
        assert_eq!(db.count("t").unwrap(), 2);
    }

    #[test]
    fn metrics() {
        let (_d, db) = temp_db();
        assert_eq!(db.get_metric("c").unwrap(), 0);
        db.set_metric("c", 42).unwrap();
        assert_eq!(db.get_metric("c").unwrap(), 42);
        assert_eq!(db.inc_metric("c", 8).unwrap(), 50);
    }

    #[test]
    fn batch() {
        let (_d, db) = temp_db();
        db.batch(|w| {
            w.set("t", "a", &"1")?;
            w.set("t", "b", &"2")?;
            w.set_metric("count", 2)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(db.count("t").unwrap(), 2);
        assert_eq!(db.get_metric("count").unwrap(), 2);
    }

    #[test]
    fn next_counter() {
        let (_d, db) = temp_db();
        assert_eq!(db.next_counter("counters", "vni", 100).unwrap(), 100);
        assert_eq!(db.next_counter("counters", "vni", 100).unwrap(), 101);
        assert_eq!(db.next_counter("counters", "vni", 100).unwrap(), 102);
    }

    #[test]
    fn overwrite() {
        let (_d, db) = temp_db();
        db.set("t", "k", &"old").unwrap();
        db.set("t", "k", &"new").unwrap();
        assert_eq!(db.get::<String>("t", "k").unwrap(), Some("new".into()));
    }

    #[test]
    fn export_import() {
        let (_d, db) = temp_db();
        db.set("t", "a", &"1").unwrap();
        db.set("t", "b", &"2").unwrap();

        let exported = db.export_raw("t").unwrap();
        assert_eq!(exported.len(), 2);

        // Clear and reimport
        db.delete("t", "a").unwrap();
        db.delete("t", "b").unwrap();
        assert_eq!(db.count("t").unwrap(), 0);

        db.import_raw("t", &exported).unwrap();
        assert_eq!(db.count("t").unwrap(), 2);
        assert_eq!(db.get::<String>("t", "a").unwrap(), Some("1".into()));
    }

    #[test]
    fn layer_name() {
        let (_d, db) = temp_db();
        assert_eq!(db.layer(), "test");
    }
}
