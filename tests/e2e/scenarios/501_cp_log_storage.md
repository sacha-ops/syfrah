# E2E: Control Plane Log Storage

**ID**: 501_cp_log_storage
**Layer**: controlplane
**Priority**: P0

## Objective
Verify the Raft log storage implementation backed by redb correctly persists and restores log entries, votes, and committed state.

## Prerequisites
- Control plane crate builds and passes tests

## Steps

1. **RaftLogStorage trait implemented**
   Verify `Arc<RedbLogStore>` implements `RaftLogStorage<SyfrahRaftConfig>` with all required methods:
   - `get_log_state` — returns last_purged and last_log_id
   - `get_log_reader` — returns a clone of self
   - `save_vote` / `read_vote` — persists current vote to redb
   - `save_committed` / `read_committed` — persists committed log ID
   - `append` — writes entries to both in-memory BTreeMap and redb
   - `truncate_after` — removes entries after a given log ID
   - `purge` — removes entries up to a given log ID

2. **RaftLogReader trait implemented**
   Verify `Arc<RedbLogStore>` implements `RaftLogReader<SyfrahRaftConfig>`:
   - `try_get_log_entries` — reads entries for a given range
   - `read_vote` — reads persisted vote
   - `limited_get_log_entries` — reads entries for a range

3. **Vote persistence across restarts**
   ```bash
   cargo test -p syfrah-controlplane -- log_storage::tests::vote_persistence
   cargo test -p syfrah-controlplane -- log_storage::tests::log_store_restores_from_redb
   ```
   Expected: votes survive store reconstruction

4. **Empty state initialization**
   ```bash
   cargo test -p syfrah-controlplane -- log_storage::tests::log_store_empty_state
   ```
   Expected: fresh store has no logs, no purged, no vote

5. **Committed state tracking**
   ```bash
   cargo test -p syfrah-controlplane -- log_storage::tests::committed_persistence
   ```

## Pass criteria
- All unit tests pass
- No clippy warnings
- Vote data survives store reconstruction (redb persistence)
