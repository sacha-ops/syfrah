#![allow(clippy::result_large_err)]
//! Embedded state persistence for Syfrah layers.
//!
//! Each layer gets its own redb database file at `~/.syfrah/{layer}.redb`.
//!
//! # Features
//!
//! - **Typed tables**: compile-time safe table definitions
//! - **Prefix scan**: list entries by key prefix
//! - **Pagination**: `list_range` with limit
//! - **Secondary indexes**: automatic index maintenance
//! - **TTL / expiration**: entries with time-to-live
//! - **Watch / notifications**: subscribe to key changes
//! - **Optimistic locking**: compare-and-swap with versions
//! - **Compaction**: GC for deleted/expired entries
//! - **Atomic batches**: multi-table ACID transactions
//! - **Metrics**: u64 counters
//! - **Export/Import**: snapshot support
//!
//! # Usage
//!
//! ```no_run
//! use syfrah_state::{LayerDb, TypedTable};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Peer { name: String }
//!
//! const PEERS: TypedTable<Peer> = TypedTable::new("peers");
//!
//! let db = LayerDb::open("fabric").unwrap();
//! db.put(&PEERS, "node-1", &Peer { name: "node-1".into() }).unwrap();
//! let peer = db.fetch(&PEERS, "node-1").unwrap();
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::de::DeserializeOwned;
use serde::Serialize;

// ═══════════════════════════════════════════════════
// Errors
// ═══════════════════════════════════════════════════

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
    #[error("version conflict: expected version {expected}, found {found}")]
    VersionConflict { expected: u64, found: u64 },
    #[error("key expired")]
    Expired,
}

pub type Result<T> = std::result::Result<T, StateError>;

// ═══════════════════════════════════════════════════
// 1. Typed Tables — compile-time safe table references
// ═══════════════════════════════════════════════════

/// A compile-time typed table reference.
///
/// ```
/// use syfrah_state::TypedTable;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct Vpc { name: String, cidr: String }
///
/// const VPCS: TypedTable<Vpc> = TypedTable::new("vpcs");
/// ```
pub struct TypedTable<T> {
    name: &'static str,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> TypedTable<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

// ═══════════════════════════════════════════════════
// Versioned entry — for optimistic locking
// ═══════════════════════════════════════════════════

/// A value wrapper that includes a version number and optional TTL.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct Envelope<T> {
    pub value: T,
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>, // unix epoch seconds
}

impl<T> Envelope<T> {
    fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now >= exp
        } else {
            false
        }
    }
}

// ═══════════════════════════════════════════════════
// Watch — key change notifications
// ═══════════════════════════════════════════════════

type WatchCallback = Box<dyn Fn(&str, &str) + Send + Sync>; // (table, key)

// ═══════════════════════════════════════════════════
// LayerDb
// ═══════════════════════════════════════════════════

fn syfrah_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
}

pub fn db_path(layer: &str) -> PathBuf {
    syfrah_dir().join(format!("{layer}.redb"))
}

/// Per-layer state database backed by redb.
#[derive(Clone)]
pub struct LayerDb {
    db: Arc<Database>,
    layer: String,
    watchers: Arc<Mutex<Vec<(String, WatchCallback)>>>,
}

impl std::fmt::Debug for LayerDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerDb")
            .field("layer", &self.layer)
            .finish()
    }
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
            watchers: Arc::new(Mutex::new(Vec::new())),
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
            watchers: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn layer(&self) -> &str {
        &self.layer
    }

    // ── Typed table operations ─────────────────────────────

    /// Get a value using a typed table. Respects TTL.
    pub fn fetch<T: DeserializeOwned>(
        &self,
        table: &TypedTable<T>,
        key: &str,
    ) -> Result<Option<T>> {
        let env: Option<Envelope<T>> = self.get_raw(table.name, key)?;
        match env {
            Some(e) if e.is_expired() => Ok(None),
            Some(e) => Ok(Some(e.value)),
            None => Ok(None),
        }
    }

    /// Get a value with its version number.
    pub fn fetch_versioned<T: DeserializeOwned>(
        &self,
        table: &TypedTable<T>,
        key: &str,
    ) -> Result<Option<(T, u64)>> {
        let env: Option<Envelope<T>> = self.get_raw(table.name, key)?;
        match env {
            Some(e) if e.is_expired() => Ok(None),
            Some(e) => Ok(Some((e.value, e.version))),
            None => Ok(None),
        }
    }

    /// Put a value using a typed table. Auto-increments version.
    pub fn put<T: Serialize + DeserializeOwned>(
        &self,
        table: &TypedTable<T>,
        key: &str,
        value: &T,
    ) -> Result<u64> {
        let version = self.next_version(table.name, key)?;
        let env = Envelope {
            value,
            version,
            expires_at: None,
        };
        self.set_raw(table.name, key, &env)?;
        self.notify_watchers(table.name, key);
        Ok(version)
    }

    /// Put a value with a TTL (seconds from now).
    pub fn put_with_ttl<T: Serialize + DeserializeOwned>(
        &self,
        table: &TypedTable<T>,
        key: &str,
        value: &T,
        ttl_secs: u64,
    ) -> Result<u64> {
        let version = self.next_version(table.name, key)?;
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + ttl_secs;
        let env = Envelope {
            value,
            version,
            expires_at: Some(expires_at),
        };
        self.set_raw(table.name, key, &env)?;
        self.notify_watchers(table.name, key);
        Ok(version)
    }

    /// Compare-and-swap: set value only if current version matches expected.
    pub fn cas<T: Serialize + DeserializeOwned>(
        &self,
        table: &TypedTable<T>,
        key: &str,
        value: &T,
        expected_version: u64,
    ) -> Result<u64> {
        let current: Option<Envelope<serde_json::Value>> = self.get_raw(table.name, key)?;
        let current_version = current.map(|e| e.version).unwrap_or(0);

        if current_version != expected_version {
            return Err(StateError::VersionConflict {
                expected: expected_version,
                found: current_version,
            });
        }

        let new_version = expected_version + 1;
        let env = Envelope {
            value,
            version: new_version,
            expires_at: None,
        };
        self.set_raw(table.name, key, &env)?;
        self.notify_watchers(table.name, key);
        Ok(new_version)
    }

    /// Remove a key from a typed table.
    pub fn remove<T>(&self, table: &TypedTable<T>, key: &str) -> Result<bool> {
        let existed = self.delete(table.name, key)?;
        if existed {
            self.notify_watchers(table.name, key);
        }
        Ok(existed)
    }

    /// List all non-expired entries in a typed table.
    pub fn list_all<T: DeserializeOwned>(&self, table: &TypedTable<T>) -> Result<Vec<(String, T)>> {
        let envs: Vec<(String, Envelope<T>)> = self.list(table.name)?;
        Ok(envs
            .into_iter()
            .filter(|(_, e)| !e.is_expired())
            .map(|(k, e)| (k, e.value))
            .collect())
    }

    // ── 2. Prefix scan ─────────────────────────────────────

    /// List entries whose keys start with `prefix`.
    pub fn list_by_prefix<T: DeserializeOwned>(
        &self,
        table: &str,
        prefix: &str,
    ) -> Result<Vec<(String, T)>> {
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
            let key = k.value().to_string();
            if key.starts_with(prefix) {
                results.push((key, serde_json::from_slice(v.value())?));
            }
        }
        Ok(results)
    }

    /// Prefix scan on typed table (filters expired).
    pub fn scan_prefix<T: DeserializeOwned>(
        &self,
        table: &TypedTable<T>,
        prefix: &str,
    ) -> Result<Vec<(String, T)>> {
        let envs: Vec<(String, Envelope<T>)> = self.list_by_prefix(table.name, prefix)?;
        Ok(envs
            .into_iter()
            .filter(|(_, e)| !e.is_expired())
            .map(|(k, e)| (k, e.value))
            .collect())
    }

    // ── 3. Pagination ──────────────────────────────────────

    /// List entries with offset and limit.
    pub fn list_page<T: DeserializeOwned>(
        &self,
        table: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<(String, T)>> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(StateError::Table(e)),
        };

        let mut results = Vec::new();
        for (i, entry) in t.iter().map_err(StateError::Storage)?.enumerate() {
            if i < offset {
                continue;
            }
            if results.len() >= limit {
                break;
            }
            let (k, v) = entry.map_err(StateError::Storage)?;
            results.push((k.value().to_string(), serde_json::from_slice(v.value())?));
        }
        Ok(results)
    }

    // ── 4. Secondary indexes ───────────────────────────────

    /// Set a value with a secondary index entry.
    /// Writes both the primary entry and the index entry atomically.
    pub fn set_with_index<T: Serialize>(
        &self,
        table: &str,
        key: &str,
        value: &T,
        index_table: &str,
        index_key: &str,
    ) -> Result<()> {
        self.batch(|w| {
            w.set(table, key, value)?;
            // Index value points to the primary key
            w.set(index_table, index_key, &key.to_string())?;
            Ok(())
        })
    }

    /// Delete a value and its index entry atomically.
    pub fn delete_with_index(
        &self,
        table: &str,
        key: &str,
        index_table: &str,
        index_key: &str,
    ) -> Result<()> {
        self.batch(|w| {
            w.delete(table, key)?;
            w.delete(index_table, index_key)?;
            Ok(())
        })
    }

    /// Lookup a primary key via an index.
    pub fn lookup_index(&self, index_table: &str, index_key: &str) -> Result<Option<String>> {
        self.get::<String>(index_table, index_key)
    }

    // ── 6. Watch / notifications ───────────────────────────

    /// Register a callback to be called when a key in `table` changes.
    /// Returns a watch ID that can be used to unwatch.
    pub fn watch(
        &self,
        table: impl Into<String>,
        callback: impl Fn(&str, &str) + Send + Sync + 'static,
    ) -> usize {
        let mut watchers = self.watchers.lock().unwrap();
        let id = watchers.len();
        watchers.push((table.into(), Box::new(callback)));
        id
    }

    /// Remove a watch by ID.
    pub fn unwatch(&self, id: usize) {
        let mut watchers = self.watchers.lock().unwrap();
        if id < watchers.len() {
            // Replace with a no-op instead of removing to keep IDs stable
            watchers[id] = ("__removed__".to_string(), Box::new(|_, _| {}));
        }
    }

    fn notify_watchers(&self, table: &str, key: &str) {
        let watchers = self.watchers.lock().unwrap();
        for (watch_table, callback) in watchers.iter() {
            if watch_table == table {
                callback(table, key);
            }
        }
    }

    // ── 8. Compaction / GC ─────────────────────────────────

    /// Remove all expired entries from a table. Returns number removed.
    pub fn gc_expired(&self, table: &str) -> Result<usize> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        let t = match txn.open_table(td) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(e) => return Err(StateError::Table(e)),
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut expired_keys = Vec::new();
        for entry in t.iter().map_err(StateError::Storage)? {
            let (k, v) = entry.map_err(StateError::Storage)?;
            // Try to parse as envelope to check expiry
            if let Ok(env) = serde_json::from_slice::<Envelope<serde_json::Value>>(v.value()) {
                if let Some(exp) = env.expires_at {
                    if now >= exp {
                        expired_keys.push(k.value().to_string());
                    }
                }
            }
        }
        drop(t);
        drop(txn);

        let count = expired_keys.len();
        if !expired_keys.is_empty() {
            self.batch(|w| {
                for key in &expired_keys {
                    w.delete(table, key)?;
                }
                Ok(())
            })?;
        }
        Ok(count)
    }

    // ── Raw (untyped) operations — kept for backward compat ──

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

    pub fn set<T: Serialize>(&self, table: &str, key: &str, value: &T) -> Result<()> {
        self.set_raw(table, key, value)
    }

    fn set_raw<T: Serialize>(&self, table: &str, key: &str, value: &T) -> Result<()> {
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

    fn get_raw<T: DeserializeOwned>(&self, table: &str, key: &str) -> Result<Option<T>> {
        self.get(table, key)
    }

    fn next_version(&self, table: &str, key: &str) -> Result<u64> {
        let current: Option<Envelope<serde_json::Value>> = self.get_raw(table, key)?;
        Ok(current.map(|e| e.version + 1).unwrap_or(1))
    }

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

    pub fn count(&self, table: &str) -> Result<u64> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_read()?;

        match txn.open_table(td) {
            Ok(t) => t.len().map_err(StateError::Storage),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
            Err(redb::TableError::TableTypeMismatch { .. }) => {
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

    pub fn import_raw(&self, table: &str, entries: &[(String, Vec<u8>)]) -> Result<()> {
        let td: TableDefinition<&str, &[u8]> = TableDefinition::new(table);
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(td)?;
            let keys: Vec<String> = t
                .iter()
                .map_err(StateError::Storage)?
                .filter_map(|e| e.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for key in &keys {
                t.remove(key.as_str())?;
            }
            for (k, v) in entries {
                t.insert(k.as_str(), v.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn destroy(layer: &str) -> Result<()> {
        let path = db_path(layer);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn layer_exists(layer: &str) -> bool {
        db_path(layer).exists()
    }
}

/// Writer within a batch transaction.
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestPeer {
        name: String,
    }

    const PEERS: TypedTable<TestPeer> = TypedTable::new("peers");

    fn temp_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    // ── Basic raw operations (backward compat) ──

    #[test]
    fn raw_set_and_get() {
        let (_d, db) = temp_db();
        db.set("t", "k", &"v").unwrap();
        assert_eq!(db.get::<String>("t", "k").unwrap(), Some("v".into()));
    }

    #[test]
    fn raw_get_missing() {
        let (_d, db) = temp_db();
        assert_eq!(db.get::<String>("t", "k").unwrap(), None);
    }

    #[test]
    fn raw_delete() {
        let (_d, db) = temp_db();
        db.set("t", "k", &"v").unwrap();
        assert!(db.delete("t", "k").unwrap());
        assert!(!db.delete("t", "k").unwrap());
    }

    #[test]
    fn raw_list() {
        let (_d, db) = temp_db();
        db.set("t", "a", &"1").unwrap();
        db.set("t", "b", &"2").unwrap();
        assert_eq!(db.list::<String>("t").unwrap().len(), 2);
    }

    #[test]
    fn raw_exists() {
        let (_d, db) = temp_db();
        assert!(!db.exists("t", "k").unwrap());
        db.set("t", "k", &"v").unwrap();
        assert!(db.exists("t", "k").unwrap());
    }

    #[test]
    fn raw_count() {
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
    fn batch_atomic() {
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
        assert_eq!(db.next_counter("c", "vni", 100).unwrap(), 100);
        assert_eq!(db.next_counter("c", "vni", 100).unwrap(), 101);
    }

    #[test]
    fn export_import() {
        let (_d, db) = temp_db();
        db.set("t", "a", &"1").unwrap();
        let exported = db.export_raw("t").unwrap();
        db.delete("t", "a").unwrap();
        db.import_raw("t", &exported).unwrap();
        assert_eq!(db.get::<String>("t", "a").unwrap(), Some("1".into()));
    }

    // ── 1. Typed tables ──

    #[test]
    fn typed_put_and_fetch() {
        let (_d, db) = temp_db();
        let peer = TestPeer { name: "n1".into() };
        let v = db.put(&PEERS, "n1", &peer).unwrap();
        assert_eq!(v, 1);
        assert_eq!(db.fetch(&PEERS, "n1").unwrap(), Some(peer));
    }

    #[test]
    fn typed_fetch_missing() {
        let (_d, db) = temp_db();
        assert_eq!(db.fetch::<TestPeer>(&PEERS, "nope").unwrap(), None);
    }

    #[test]
    fn typed_version_auto_increments() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        assert_eq!(db.put(&PEERS, "n1", &p).unwrap(), 1);
        assert_eq!(db.put(&PEERS, "n1", &p).unwrap(), 2);
        assert_eq!(db.put(&PEERS, "n1", &p).unwrap(), 3);
    }

    #[test]
    fn typed_fetch_versioned() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        db.put(&PEERS, "n1", &p).unwrap();
        let (val, ver) = db.fetch_versioned(&PEERS, "n1").unwrap().unwrap();
        assert_eq!(val, p);
        assert_eq!(ver, 1);
    }

    #[test]
    fn typed_remove() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        db.put(&PEERS, "n1", &p).unwrap();
        assert!(db.remove(&PEERS, "n1").unwrap());
        assert_eq!(db.fetch::<TestPeer>(&PEERS, "n1").unwrap(), None);
    }

    #[test]
    fn typed_list_all() {
        let (_d, db) = temp_db();
        db.put(&PEERS, "a", &TestPeer { name: "a".into() }).unwrap();
        db.put(&PEERS, "b", &TestPeer { name: "b".into() }).unwrap();
        assert_eq!(db.list_all(&PEERS).unwrap().len(), 2);
    }

    // ── 2. Prefix scan ──

    #[test]
    fn prefix_scan() {
        let (_d, db) = temp_db();
        db.set("vpcs", "eu/vpc-1", &"v1").unwrap();
        db.set("vpcs", "eu/vpc-2", &"v2").unwrap();
        db.set("vpcs", "us/vpc-3", &"v3").unwrap();

        let eu: Vec<(String, String)> = db.list_by_prefix("vpcs", "eu/").unwrap();
        assert_eq!(eu.len(), 2);

        let us: Vec<(String, String)> = db.list_by_prefix("vpcs", "us/").unwrap();
        assert_eq!(us.len(), 1);

        let all: Vec<(String, String)> = db.list_by_prefix("vpcs", "").unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn typed_prefix_scan() {
        let (_d, db) = temp_db();
        db.put(&PEERS, "eu/n1", &TestPeer { name: "n1".into() })
            .unwrap();
        db.put(&PEERS, "eu/n2", &TestPeer { name: "n2".into() })
            .unwrap();
        db.put(&PEERS, "us/n3", &TestPeer { name: "n3".into() })
            .unwrap();

        let eu = db.scan_prefix(&PEERS, "eu/").unwrap();
        assert_eq!(eu.len(), 2);
    }

    // ── 3. Pagination ──

    #[test]
    fn pagination() {
        let (_d, db) = temp_db();
        for i in 0..10 {
            db.set("t", &format!("k{i:02}"), &i).unwrap();
        }

        let page1: Vec<(String, i32)> = db.list_page("t", 0, 3).unwrap();
        assert_eq!(page1.len(), 3);

        let page2: Vec<(String, i32)> = db.list_page("t", 3, 3).unwrap();
        assert_eq!(page2.len(), 3);

        // Pages don't overlap
        assert_ne!(page1[0].0, page2[0].0);

        let beyond: Vec<(String, i32)> = db.list_page("t", 20, 5).unwrap();
        assert_eq!(beyond.len(), 0);
    }

    // ── 4. Secondary indexes ──

    #[test]
    fn secondary_index() {
        let (_d, db) = temp_db();
        db.set_with_index("vpcs", "vpc-01", &"my-vpc", "vpcs_by_name", "my-vpc")
            .unwrap();

        // Lookup by name
        let id = db.lookup_index("vpcs_by_name", "my-vpc").unwrap().unwrap();
        assert_eq!(id, "vpc-01");

        // Get by ID
        let name: String = db.get("vpcs", "vpc-01").unwrap().unwrap();
        assert_eq!(name, "my-vpc");

        // Delete both atomically
        db.delete_with_index("vpcs", "vpc-01", "vpcs_by_name", "my-vpc")
            .unwrap();
        assert_eq!(db.get::<String>("vpcs", "vpc-01").unwrap(), None);
        assert_eq!(db.lookup_index("vpcs_by_name", "my-vpc").unwrap(), None);
    }

    // ── 5. TTL / expiration ──

    #[test]
    fn ttl_not_expired() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        db.put_with_ttl(&PEERS, "n1", &p, 3600).unwrap(); // 1 hour
        assert_eq!(db.fetch(&PEERS, "n1").unwrap(), Some(p));
    }

    #[test]
    fn ttl_expired() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        // TTL of 0 seconds = already expired
        db.put_with_ttl(&PEERS, "n1", &p, 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(db.fetch::<TestPeer>(&PEERS, "n1").unwrap(), None);
    }

    #[test]
    fn ttl_filtered_from_list() {
        let (_d, db) = temp_db();
        db.put(&PEERS, "alive", &TestPeer { name: "a".into() })
            .unwrap();
        db.put_with_ttl(&PEERS, "expired", &TestPeer { name: "e".into() }, 0)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let all = db.list_all(&PEERS).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "alive");
    }

    // ── 6. Watch ──

    #[test]
    fn watch_notifies_on_put() {
        let (_d, db) = temp_db();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        db.watch("peers", move |_table, _key| {
            c.fetch_add(1, Ordering::SeqCst);
        });

        let p = TestPeer { name: "n1".into() };
        db.put(&PEERS, "n1", &p).unwrap();
        db.put(&PEERS, "n2", &p).unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn watch_notifies_on_remove() {
        let (_d, db) = temp_db();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        db.watch("peers", move |_table, _key| {
            c.fetch_add(1, Ordering::SeqCst);
        });

        let p = TestPeer { name: "n1".into() };
        db.put(&PEERS, "n1", &p).unwrap();
        db.remove(&PEERS, "n1").unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 2); // put + remove
    }

    #[test]
    fn unwatch_stops_notifications() {
        let (_d, db) = temp_db();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let id = db.watch("peers", move |_, _| {
            c.fetch_add(1, Ordering::SeqCst);
        });

        let p = TestPeer { name: "n1".into() };
        db.put(&PEERS, "n1", &p).unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        db.unwatch(id);
        db.put(&PEERS, "n2", &p).unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1); // no more notifications
    }

    // ── 7. Optimistic locking (CAS) ──

    #[test]
    fn cas_succeeds() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        let v1 = db.put(&PEERS, "n1", &p).unwrap();
        assert_eq!(v1, 1);

        let p2 = TestPeer {
            name: "n1-updated".into(),
        };
        let v2 = db.cas(&PEERS, "n1", &p2, 1).unwrap();
        assert_eq!(v2, 2);

        let fetched = db.fetch(&PEERS, "n1").unwrap().unwrap();
        assert_eq!(fetched.name, "n1-updated");
    }

    #[test]
    fn cas_fails_on_version_mismatch() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "n1".into() };
        db.put(&PEERS, "n1", &p).unwrap(); // version 1

        let p2 = TestPeer {
            name: "stale".into(),
        };
        let result = db.cas(&PEERS, "n1", &p2, 0); // wrong version
        assert!(matches!(result, Err(StateError::VersionConflict { .. })));
    }

    #[test]
    fn cas_on_nonexistent_key() {
        let (_d, db) = temp_db();
        let p = TestPeer { name: "new".into() };
        // Version 0 means "key doesn't exist"
        let v = db.cas(&PEERS, "new-key", &p, 0).unwrap();
        assert_eq!(v, 1);
    }

    // ── 8. GC / compaction ──

    #[test]
    fn gc_removes_expired() {
        let (_d, db) = temp_db();
        db.put(&PEERS, "alive", &TestPeer { name: "a".into() })
            .unwrap();
        db.put_with_ttl(&PEERS, "dead1", &TestPeer { name: "d1".into() }, 0)
            .unwrap();
        db.put_with_ttl(&PEERS, "dead2", &TestPeer { name: "d2".into() }, 0)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let removed = db.gc_expired("peers").unwrap();
        assert_eq!(removed, 2);

        // Only alive remains
        assert_eq!(db.count("peers").unwrap(), 1);
        assert!(db.exists("peers", "alive").unwrap());
    }

    #[test]
    fn gc_empty_table() {
        let (_d, db) = temp_db();
        assert_eq!(db.gc_expired("empty").unwrap(), 0);
    }

    // ── Layer name ──

    #[test]
    fn layer_name() {
        let (_d, db) = temp_db();
        assert_eq!(db.layer(), "test");
    }
}
