//! Idempotency journal — request deduplication in the state machine.
//!
//! Maintains a time-limited cache of recently applied idempotency keys and their
//! results. When the same key appears again:
//! - Same key + same payload fingerprint → return cached result (dedup)
//! - Same key + different payload fingerprint → return 409 Conflict
//!
//! Keys expire after 24 hours and are garbage collected during snapshots.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::commands::StateMachineResponse;

/// TTL for idempotency keys: 24 hours.
const KEY_TTL_SECS: u64 = 24 * 3600;

/// A cached result from a previously applied command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The idempotency key.
    pub key: String,
    /// Fingerprint of the command payload (hash of serialized command).
    pub payload_fingerprint: u64,
    /// The cached response.
    pub response: StateMachineResponse,
    /// Unix timestamp when the entry was created.
    pub created_at: u64,
}

/// Result of checking the idempotency journal.
pub enum IdempotencyCheck {
    /// Key not seen before — proceed with execution.
    New,
    /// Key seen with same payload — return cached result.
    Duplicate(StateMachineResponse),
    /// Key seen with different payload — conflict.
    Conflict,
}

/// In-memory idempotency journal with TTL-based expiration.
pub struct IdempotencyJournal {
    entries: Mutex<HashMap<String, JournalEntry>>,
}

impl IdempotencyJournal {
    /// Create a new empty journal.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Check if an idempotency key has been seen before.
    pub fn check(&self, key: &str, payload_fingerprint: u64) -> IdempotencyCheck {
        let entries = self.entries.lock().unwrap();
        match entries.get(key) {
            None => IdempotencyCheck::New,
            Some(entry) => {
                // Check if expired.
                let now = now_secs();
                if now.saturating_sub(entry.created_at) > KEY_TTL_SECS {
                    return IdempotencyCheck::New; // Expired — treat as new.
                }
                if entry.payload_fingerprint == payload_fingerprint {
                    IdempotencyCheck::Duplicate(entry.response.clone())
                } else {
                    IdempotencyCheck::Conflict
                }
            }
        }
    }

    /// Record a result for an idempotency key.
    pub fn record(&self, key: String, payload_fingerprint: u64, response: StateMachineResponse) {
        let entry = JournalEntry {
            key: key.clone(),
            payload_fingerprint,
            response,
            created_at: now_secs(),
        };
        let mut entries = self.entries.lock().unwrap();
        entries.insert(key, entry);
    }

    /// Garbage collect expired entries. Called during snapshot.
    pub fn gc(&self) -> usize {
        let now = now_secs();
        let mut entries = self.entries.lock().unwrap();
        let before = entries.len();
        entries.retain(|_, e| now.saturating_sub(e.created_at) <= KEY_TTL_SECS);
        before - entries.len()
    }

    /// Serialize the journal for inclusion in a Raft snapshot.
    pub fn to_entries(&self) -> Vec<JournalEntry> {
        let entries = self.entries.lock().unwrap();
        entries.values().cloned().collect()
    }

    /// Restore the journal from a Raft snapshot.
    pub fn restore(&self, entries: Vec<JournalEntry>) {
        let mut journal = self.entries.lock().unwrap();
        journal.clear();
        for entry in entries {
            journal.insert(entry.key.clone(), entry);
        }
    }

    /// Number of entries currently in the journal.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// Check if the journal is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().unwrap().is_empty()
    }
}

impl Default for IdempotencyJournal {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Compute a payload fingerprint from a serializable command.
pub fn fingerprint(cmd: &impl serde::Serialize) -> u64 {
    use std::hash::{Hash, Hasher};
    let json = serde_json::to_string(cmd).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::StateMachineResponse;

    #[test]
    fn new_key_returns_new() {
        let journal = IdempotencyJournal::new();
        match journal.check("key-1", 12345) {
            IdempotencyCheck::New => {}
            other => panic!(
                "expected New, got {other:?}",
                other = std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn duplicate_key_same_payload_returns_cached() {
        let journal = IdempotencyJournal::new();
        journal.record(
            "key-1".to_string(),
            12345,
            StateMachineResponse::Created("id-1".to_string()),
        );
        match journal.check("key-1", 12345) {
            IdempotencyCheck::Duplicate(resp) => {
                assert!(matches!(resp, StateMachineResponse::Created(ref id) if id == "id-1"));
            }
            _ => panic!("expected Duplicate"),
        }
    }

    #[test]
    fn duplicate_key_different_payload_returns_conflict() {
        let journal = IdempotencyJournal::new();
        journal.record("key-1".to_string(), 12345, StateMachineResponse::Ok);
        match journal.check("key-1", 99999) {
            IdempotencyCheck::Conflict => {}
            _ => panic!("expected Conflict"),
        }
    }

    #[test]
    fn gc_removes_expired() {
        let journal = IdempotencyJournal::new();
        // Manually insert an expired entry.
        {
            let mut entries = journal.entries.lock().unwrap();
            entries.insert(
                "old-key".to_string(),
                JournalEntry {
                    key: "old-key".to_string(),
                    payload_fingerprint: 1,
                    response: StateMachineResponse::Ok,
                    created_at: 0, // Very old.
                },
            );
        }
        let removed = journal.gc();
        assert_eq!(removed, 1);
        assert!(journal.is_empty());
    }

    #[test]
    fn snapshot_roundtrip() {
        let journal = IdempotencyJournal::new();
        journal.record("k1".to_string(), 1, StateMachineResponse::Ok);
        journal.record(
            "k2".to_string(),
            2,
            StateMachineResponse::Created("x".to_string()),
        );

        let entries = journal.to_entries();
        assert_eq!(entries.len(), 2);

        let journal2 = IdempotencyJournal::new();
        journal2.restore(entries);
        assert_eq!(journal2.len(), 2);
    }
}
