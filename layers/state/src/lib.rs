//! Embedded state persistence for Syfrah layers.
//!
//! Each layer gets its own redb database file at `~/.syfrah/{layer}.redb`.
//! This crate provides a thin wrapper around redb that enforces conventions:
//! - One file per layer
//! - JSON serialization for values
//! - Typed get/set/delete/list operations
//! - Arc-safe database handle for async sharing

pub mod cli;

use std::path::PathBuf;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
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

/// The base directory for all syfrah state files.
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
/// Each layer opens its own `LayerDb` with a unique name.
///
/// # Example
///
/// ```no_run
/// use syfrah_state::LayerDb;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct Peer { name: String, endpoint: String }
///
/// let db = LayerDb::open("fabric").unwrap();
/// db.set("peers", "key1", &Peer { name: "a".into(), endpoint: "1.2.3.4:51820".into() }).unwrap();
/// let peer: Option<Peer> = db.get("peers", "key1").unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct LayerDb {
    db: Arc<Database>,
    layer: String,
}

impl LayerDb {
    /// Open (or create) the redb database for a layer.
    ///
    /// Creates `~/.syfrah/` if it doesn't exist.
    /// Creates `~/.syfrah/{layer}.redb` if it doesn't exist.
    pub fn open(layer: &str) -> Result<Self> {
        let dir = syfrah_dir();
        std::fs::create_dir_all(&dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
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
            .unwrap_or("unknown")
            .to_string();
        Ok(Self {
            db: Arc::new(db),
            layer,
        })
    }

    /// Get the layer name.
    pub fn layer(&self) -> &str {
        &self.layer
    }

    /// Get a value by key from a table. Returns `None` if the key doesn't exist.
    pub fn get<T: DeserializeOwned>(&self, table_name: &str, key: &str) -> Result<Option<T>> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(StateError::Table(e)),
        };

        let access = table.get(key).map_err(StateError::Storage)?;
        match access {
            Some(value) => {
                let bytes = value.value();
                let v: T = serde_json::from_slice(bytes)?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// Set a value by key in a table. Creates the table if it doesn't exist.
    pub fn set<T: Serialize>(&self, table_name: &str, key: &str, value: &T) -> Result<()> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let bytes = serde_json::to_vec(value)?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(table_def)?;
            table.insert(key, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete a key from a table. Returns true if the key existed.
    pub fn delete(&self, table_name: &str, key: &str) -> Result<bool> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);

        let write_txn = self.db.begin_write()?;
        let existed;
        {
            let mut table = write_txn.open_table(table_def)?;
            let removed = table.remove(key)?;
            existed = removed.is_some();
            drop(removed);
        }
        write_txn.commit()?;
        Ok(existed)
    }

    /// List all key-value pairs in a table.
    pub fn list<T: DeserializeOwned>(&self, table_name: &str) -> Result<Vec<(String, T)>> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(StateError::Table(e)),
        };

        let mut results = Vec::new();
        for entry in table.iter().map_err(StateError::Storage)? {
            let (key, value) = entry.map_err(StateError::Storage)?;
            let k = key.value().to_string();
            let v: T = serde_json::from_slice(value.value())?;
            results.push((k, v));
        }
        Ok(results)
    }

    /// Count the number of keys in a table.
    ///
    /// Tries `Table<&str, &[u8]>` first; if the table uses a different value
    /// type (e.g. `u64` for the metrics table), falls back to `Table<&str, u64>`.
    pub fn count(&self, table_name: &str) -> Result<u64> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        match read_txn.open_table(table_def) {
            Ok(t) => return t.len().map_err(StateError::Storage),
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(redb::TableError::TableTypeMismatch { .. }) => {
                // Fall through to try u64 value type.
            }
            Err(e) => return Err(StateError::Table(e)),
        }

        // Retry with u64 value type (used by the metrics table).
        let table_def_u64: TableDefinition<&str, u64> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def_u64) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(e) => return Err(StateError::Table(e)),
        };

        table.len().map_err(StateError::Storage)
    }

    /// Count keys and check existence in a single read transaction.
    ///
    /// Like [`count`], falls back to `Table<&str, u64>` on type mismatch.
    pub fn count_and_exists(&self, table_name: &str, key: &str) -> Result<(u64, bool)> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        match read_txn.open_table(table_def) {
            Ok(t) => {
                let count = t.len().map_err(StateError::Storage)?;
                let exists = t.get(key).map_err(StateError::Storage)?.is_some();
                return Ok((count, exists));
            }
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok((0, false)),
            Err(redb::TableError::TableTypeMismatch { .. }) => {
                // Fall through to try u64 value type.
            }
            Err(e) => return Err(StateError::Table(e)),
        }

        // Retry with u64 value type (used by the metrics table).
        let table_def_u64: TableDefinition<&str, u64> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def_u64) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok((0, false)),
            Err(e) => return Err(StateError::Table(e)),
        };

        let count = table.len().map_err(StateError::Storage)?;
        let exists = table.get(key).map_err(StateError::Storage)?.is_some();
        Ok((count, exists))
    }

    /// Check if a key exists in a table.
    pub fn exists(&self, table_name: &str, key: &str) -> Result<bool> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(false),
            Err(e) => return Err(StateError::Table(e)),
        };

        Ok(table.get(key).map_err(StateError::Storage)?.is_some())
    }

    /// Get a u64 metric value. Returns 0 if not set.
    pub fn get_metric(&self, key: &str) -> Result<u64> {
        let table_def: TableDefinition<&str, u64> = TableDefinition::new("metrics");
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(e) => return Err(StateError::Table(e)),
        };

        match table.get(key).map_err(StateError::Storage)? {
            Some(v) => Ok(v.value()),
            None => Ok(0),
        }
    }

    /// Set a u64 metric value.
    pub fn set_metric(&self, key: &str, value: u64) -> Result<()> {
        let table_def: TableDefinition<&str, u64> = TableDefinition::new("metrics");

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(table_def)?;
            table.insert(key, value)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Increment a u64 metric by a delta. Returns the new value.
    pub fn inc_metric(&self, key: &str, delta: u64) -> Result<u64> {
        let table_def: TableDefinition<&str, u64> = TableDefinition::new("metrics");

        let write_txn = self.db.begin_write()?;
        let new_value = {
            let mut table = write_txn.open_table(table_def)?;
            let current = table.get(key)?.map(|v| v.value()).unwrap_or(0);
            let new_val = current + delta;
            table.insert(key, new_val)?;
            new_val
        };
        write_txn.commit()?;
        Ok(new_value)
    }

    /// Execute a batch of writes in a single ACID transaction.
    /// The closure receives a `WriteBatch` that can set/delete across tables.
    pub fn batch<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&BatchWriter) -> Result<()>,
    {
        let write_txn = self.db.begin_write()?;
        let writer = BatchWriter { txn: &write_txn };
        f(&writer)?;
        write_txn.commit()?;
        Ok(())
    }

    /// Atomically read, increment, and return a JSON-serialized counter.
    ///
    /// Performs the read and write in a single write transaction so concurrent
    /// callers cannot observe the same value. Returns the value **before** the
    /// increment (i.e. the allocated value).
    pub fn atomic_next_counter(&self, table_name: &str, key: &str, start: u32) -> Result<u32> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);

        let write_txn = self.db.begin_write()?;
        let current = {
            let table = write_txn.open_table(table_def)?;
            let access = table.get(key).map_err(StateError::Storage)?;
            match access {
                Some(v) => {
                    let val: u32 = serde_json::from_slice(v.value())?;
                    drop(v);
                    val
                }
                None => start,
            }
        };
        {
            let mut table = write_txn.open_table(table_def)?;
            let bytes = serde_json::to_vec(&(current + 1))?;
            table.insert(key, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(current)
    }

    /// Export all key-value pairs from a table as raw JSON bytes.
    ///
    /// Returns `Vec<(key, json_bytes)>` — useful for snapshot serialization
    /// where we don't need to deserialize the values.
    pub fn export_table_raw(&self, table_name: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let read_txn = self.db.begin_read()?;

        let table = match read_txn.open_table(table_def) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(StateError::Table(e)),
        };

        let mut results = Vec::new();
        for entry in table.iter().map_err(StateError::Storage)? {
            let (key, value) = entry.map_err(StateError::Storage)?;
            results.push((key.value().to_string(), value.value().to_vec()));
        }
        Ok(results)
    }

    /// Import raw key-value pairs into a table, replacing all existing data.
    ///
    /// Used during snapshot installation to restore state machine state.
    pub fn import_table_raw(&self, table_name: &str, entries: &[(String, Vec<u8>)]) -> Result<()> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let write_txn = self.db.begin_write()?;
        {
            // Clear existing data by draining the table.
            let mut table = write_txn.open_table(table_def)?;
            // Collect keys to delete first.
            let keys: Vec<String> = table
                .iter()
                .map_err(StateError::Storage)?
                .filter_map(|e| e.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for key in &keys {
                table.remove(key.as_str())?;
            }
            // Insert new data.
            for (key, value) in entries {
                table.insert(key.as_str(), value.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete the entire database file for this layer.
    pub fn destroy(layer: &str) -> Result<()> {
        let path = db_path(layer);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Check if a layer's database file exists.
    pub fn layer_exists(layer: &str) -> bool {
        db_path(layer).exists()
    }
}

/// A writer within a batch transaction.
/// All operations in a batch are committed atomically.
pub struct BatchWriter<'a> {
    txn: &'a redb::WriteTransaction,
}

impl<'a> BatchWriter<'a> {
    /// Set a value in a table within this batch.
    pub fn set<T: Serialize>(&self, table_name: &str, key: &str, value: &T) -> Result<()> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let bytes = serde_json::to_vec(value)?;
        let mut table = self.txn.open_table(table_def)?;
        table.insert(key, bytes.as_slice())?;
        Ok(())
    }

    /// Delete a key from a table within this batch.
    pub fn delete(&self, table_name: &str, key: &str) -> Result<()> {
        let table_def: TableDefinition<&str, &[u8]> = TableDefinition::new(table_name);
        let mut table = self.txn.open_table(table_def)?;
        table.remove(key)?;
        Ok(())
    }

    /// Set a metric within this batch.
    pub fn set_metric(&self, key: &str, value: u64) -> Result<()> {
        let table_def: TableDefinition<&str, u64> = TableDefinition::new("metrics");
        let mut table = self.txn.open_table(table_def)?;
        table.insert(key, value)?;
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
        endpoint: String,
    }

    fn temp_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn set_and_get() {
        let (_dir, db) = temp_db();
        let peer = TestPeer {
            name: "node-1".into(),
            endpoint: "1.2.3.4:51820".into(),
        };

        db.set("peers", "key1", &peer).unwrap();
        let result: Option<TestPeer> = db.get("peers", "key1").unwrap();
        assert_eq!(result, Some(peer));
    }

    #[test]
    fn get_missing_key() {
        let (_dir, db) = temp_db();
        let result: Option<TestPeer> = db.get("peers", "nonexistent").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn get_from_missing_table() {
        let (_dir, db) = temp_db();
        let result: Option<TestPeer> = db.get("nonexistent_table", "key").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn delete_key() {
        let (_dir, db) = temp_db();
        let peer = TestPeer {
            name: "node-1".into(),
            endpoint: "1.2.3.4:51820".into(),
        };

        db.set("peers", "key1", &peer).unwrap();
        assert!(db.delete("peers", "key1").unwrap());
        assert!(!db.delete("peers", "key1").unwrap()); // second delete returns false

        let result: Option<TestPeer> = db.get("peers", "key1").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn list_entries() {
        let (_dir, db) = temp_db();
        let p1 = TestPeer {
            name: "node-1".into(),
            endpoint: "1.2.3.4:51820".into(),
        };
        let p2 = TestPeer {
            name: "node-2".into(),
            endpoint: "5.6.7.8:51820".into(),
        };

        db.set("peers", "key1", &p1).unwrap();
        db.set("peers", "key2", &p2).unwrap();

        let entries: Vec<(String, TestPeer)> = db.list("peers").unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn list_empty_table() {
        let (_dir, db) = temp_db();
        let entries: Vec<(String, TestPeer)> = db.list("empty").unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn count_entries() {
        let (_dir, db) = temp_db();
        assert_eq!(db.count("peers").unwrap(), 0);

        db.set("peers", "a", &"val").unwrap();
        db.set("peers", "b", &"val").unwrap();
        assert_eq!(db.count("peers").unwrap(), 2);
    }

    #[test]
    fn exists_check() {
        let (_dir, db) = temp_db();
        assert!(!db.exists("peers", "key1").unwrap());
        db.set("peers", "key1", &"val").unwrap();
        assert!(db.exists("peers", "key1").unwrap());
    }

    #[test]
    fn count_and_exists_single_txn() {
        let (_dir, db) = temp_db();
        // Empty table
        let (count, exists) = db.count_and_exists("peers", "key1").unwrap();
        assert_eq!(count, 0);
        assert!(!exists);

        db.set("peers", "key1", &"val").unwrap();
        db.set("peers", "key2", &"val").unwrap();

        let (count, exists) = db.count_and_exists("peers", "key1").unwrap();
        assert_eq!(count, 2);
        assert!(exists);

        let (count, exists) = db.count_and_exists("peers", "key3").unwrap();
        assert_eq!(count, 2);
        assert!(!exists);
    }

    #[test]
    fn metrics() {
        let (_dir, db) = temp_db();

        assert_eq!(db.get_metric("counter").unwrap(), 0);
        db.set_metric("counter", 42).unwrap();
        assert_eq!(db.get_metric("counter").unwrap(), 42);

        let new_val = db.inc_metric("counter", 8).unwrap();
        assert_eq!(new_val, 50);
        assert_eq!(db.get_metric("counter").unwrap(), 50);
    }

    #[test]
    fn batch_atomic() {
        let (_dir, db) = temp_db();
        let p1 = TestPeer {
            name: "node-1".into(),
            endpoint: "1.2.3.4:51820".into(),
        };
        let p2 = TestPeer {
            name: "node-2".into(),
            endpoint: "5.6.7.8:51820".into(),
        };

        db.batch(|w| {
            w.set("peers", "key1", &p1)?;
            w.set("peers", "key2", &p2)?;
            w.set_metric("peers_discovered", 2)?;
            Ok(())
        })
        .unwrap();

        assert_eq!(db.count("peers").unwrap(), 2);
        assert_eq!(db.get_metric("peers_discovered").unwrap(), 2);
    }

    #[test]
    fn overwrite_value() {
        let (_dir, db) = temp_db();

        db.set("config", "name", &"old").unwrap();
        db.set("config", "name", &"new").unwrap();

        let val: Option<String> = db.get("config", "name").unwrap();
        assert_eq!(val, Some("new".to_string()));
    }

    #[test]
    fn destroy_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");

        {
            let _db = LayerDb::open_at(&path).unwrap();
        }

        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }
}
