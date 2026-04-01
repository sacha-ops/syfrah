//! Raft log storage backed by redb.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::io;
use std::ops::RangeBounds;
use std::sync::Arc;

use openraft::alias::{EntryOf, LogIdOf};
use openraft::entry::RaftEntry;
use openraft::storage::{IOFlushed, LogState, RaftLogReader, RaftLogStorage};
use openraft::{OptionalSend, Vote};
use serde_json;
use syfrah_state::LayerDb;
use tokio::sync::RwLock;

use crate::types::SyfrahRaftConfig;

type LeaderId = openraft::impls::leader_id_adv::LeaderId<u64, u64>;

/// Raft log storage backed by redb.
///
/// Stores log entries in a redb table keyed by log index.
/// Also persists the current vote for leader election durability.
pub struct RedbLogStore {
    db: LayerDb,
    /// In-memory log cache for fast access. Flushed to redb on append.
    log: RwLock<BTreeMap<u64, String>>,
    /// Last purged log ID.
    last_purged_log_id: RwLock<Option<LogIdOf<SyfrahRaftConfig>>>,
    /// Persisted committed log ID.
    committed: RwLock<Option<LogIdOf<SyfrahRaftConfig>>>,
    /// Current vote.
    vote: RwLock<Option<Vote<LeaderId>>>,
}

const LOG_TABLE: &str = "raft_log";
const VOTE_TABLE: &str = "raft_vote";
const META_TABLE: &str = "raft_meta";
const VOTE_KEY: &str = "current_vote";
const PURGED_KEY: &str = "last_purged";
const COMMITTED_KEY: &str = "committed";

impl RedbLogStore {
    /// Create a new log store backed by the given redb database.
    pub fn new(db: LayerDb) -> Self {
        // Restore persisted state from redb.
        let vote: Option<Vote<LeaderId>> = db.get(VOTE_TABLE, VOTE_KEY).ok().flatten();
        let last_purged: Option<LogIdOf<SyfrahRaftConfig>> =
            db.get(META_TABLE, PURGED_KEY).ok().flatten();
        let committed: Option<LogIdOf<SyfrahRaftConfig>> =
            db.get(META_TABLE, COMMITTED_KEY).ok().flatten();

        // Load all log entries into memory.
        let entries: Vec<(String, String)> = db.list(LOG_TABLE).unwrap_or_default();
        let mut log = BTreeMap::new();
        for (key, value) in entries {
            if let Ok(idx) = key.parse::<u64>() {
                log.insert(idx, value);
            }
        }

        Self {
            db,
            log: RwLock::new(log),
            last_purged_log_id: RwLock::new(last_purged),
            committed: RwLock::new(committed),
            vote: RwLock::new(vote),
        }
    }
}

impl RaftLogReader<SyfrahRaftConfig> for Arc<RedbLogStore> {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<EntryOf<SyfrahRaftConfig>>, io::Error> {
        let log = self.log.read().await;
        let mut entries = vec![];
        for (_, serialized) in log.range(range) {
            let ent: EntryOf<SyfrahRaftConfig> = serde_json::from_str(serialized)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            entries.push(ent);
        }
        Ok(entries)
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<LeaderId>>, io::Error> {
        Ok(*self.vote.read().await)
    }

    async fn limited_get_log_entries(
        &mut self,
        start: u64,
        end: u64,
    ) -> Result<Vec<EntryOf<SyfrahRaftConfig>>, io::Error> {
        self.try_get_log_entries(start..end).await
    }
}

impl RaftLogStorage<SyfrahRaftConfig> for Arc<RedbLogStore> {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<SyfrahRaftConfig>, io::Error> {
        let log = self.log.read().await;
        let last_serialized = log.iter().next_back().map(|(_, ent)| ent);

        let last = match last_serialized {
            None => None,
            Some(serialized) => {
                let ent: EntryOf<SyfrahRaftConfig> = serde_json::from_str(serialized)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                Some(ent.log_id())
            }
        };

        let last_purged = *self.last_purged_log_id.read().await;

        let last = match last {
            None => last_purged,
            Some(x) => Some(x),
        };

        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<LeaderId>) -> Result<(), io::Error> {
        let mut v = self.vote.write().await;
        *v = Some(*vote);
        self.db
            .set(VOTE_TABLE, VOTE_KEY, vote)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogIdOf<SyfrahRaftConfig>>,
    ) -> Result<(), io::Error> {
        let mut c = self.committed.write().await;
        *c = committed;
        if let Some(ref c) = committed {
            self.db
                .set(META_TABLE, COMMITTED_KEY, c)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        Ok(())
    }

    async fn read_committed(&mut self) -> Result<Option<LogIdOf<SyfrahRaftConfig>>, io::Error> {
        Ok(*self.committed.read().await)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: IOFlushed<SyfrahRaftConfig>,
    ) -> Result<(), io::Error>
    where
        I: IntoIterator<Item = EntryOf<SyfrahRaftConfig>> + OptionalSend,
    {
        let mut log = self.log.write().await;
        for entry in entries {
            let s = serde_json::to_string(&entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let idx = entry.index();
            // Persist to redb.
            self.db
                .set(LOG_TABLE, &idx.to_string(), &s)
                .map_err(|e| io::Error::other(e.to_string()))?;
            log.insert(idx, s);
        }
        callback.io_completed(Ok(()));
        Ok(())
    }

    async fn truncate_after(
        &mut self,
        last_log_id: Option<LogIdOf<SyfrahRaftConfig>>,
    ) -> Result<(), io::Error> {
        let start_index = match last_log_id {
            Some(log_id) => log_id.index() + 1,
            None => 0,
        };

        let mut log = self.log.write().await;
        let keys: Vec<u64> = log.range(start_index..).map(|(k, _)| *k).collect();
        for key in keys {
            log.remove(&key);
            let _ = self.db.delete(LOG_TABLE, &key.to_string());
        }
        Ok(())
    }

    async fn purge(&mut self, log_id: LogIdOf<SyfrahRaftConfig>) -> Result<(), io::Error> {
        {
            let mut ld = self.last_purged_log_id.write().await;
            *ld = Some(log_id);
        }
        self.db
            .set(META_TABLE, PURGED_KEY, &log_id)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let mut log = self.log.write().await;
        let keys: Vec<u64> = log.range(..=log_id.index()).map(|(k, _)| *k).collect();
        for key in keys {
            log.remove(&key);
            let _ = self.db.delete(LOG_TABLE, &key.to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_raft.redb");
        let db = LayerDb::open_at(&db_path).unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn log_store_empty_state() {
        let (_dir, db) = make_db();
        let mut store = Arc::new(RedbLogStore::new(db));
        let state = store.get_log_state().await.unwrap();
        assert!(state.last_log_id.is_none());
        assert!(state.last_purged_log_id.is_none());
    }

    #[tokio::test]
    async fn vote_persistence() {
        let (_dir, db) = make_db();
        let mut store = Arc::new(RedbLogStore::new(db));

        // Initially no vote.
        let vote = store.read_vote().await.unwrap();
        assert!(vote.is_none());

        // Save a vote.
        let v = Vote::new(1, 1);
        store.save_vote(&v).await.unwrap();

        // Read it back.
        let vote = store.read_vote().await.unwrap();
        assert!(vote.is_some());
    }

    #[tokio::test]
    async fn committed_persistence() {
        let (_dir, db) = make_db();
        let mut store = Arc::new(RedbLogStore::new(db));

        // Initially no committed.
        let committed = store.read_committed().await.unwrap();
        assert!(committed.is_none());

        // Save committed = None is a no-op.
        store.save_committed(None).await.unwrap();
        let committed = store.read_committed().await.unwrap();
        assert!(committed.is_none());
    }

    #[tokio::test]
    async fn log_store_restores_from_redb() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_raft.redb");

        // Create a store and save a vote.
        {
            let db = LayerDb::open_at(&db_path).unwrap();
            let mut store = Arc::new(RedbLogStore::new(db));
            let v = Vote::new(1, 1);
            store.save_vote(&v).await.unwrap();
        }

        // Re-open and verify the vote was restored.
        {
            let db = LayerDb::open_at(&db_path).unwrap();
            let mut store = Arc::new(RedbLogStore::new(db));
            let vote = store.read_vote().await.unwrap();
            assert!(vote.is_some(), "vote should be restored from redb");
        }
    }
}
