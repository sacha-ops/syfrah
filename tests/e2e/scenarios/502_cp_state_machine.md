# E2E: Control Plane State Machine

**ID**: 502_cp_state_machine
**Layer**: controlplane
**Priority**: P0

## Objective
Verify the Raft state machine correctly applies commands to the OrgStore, builds snapshots, and handles install_snapshot for state transfer.

## Prerequisites
- Control plane crate builds
- OrgStore works correctly

## Steps

1. **RaftStateMachine trait implemented**
   Verify `Arc<RedbStateMachine>` implements all required methods:
   - `applied_state` — returns last_applied_log and membership
   - `apply` — dispatches commands to OrgStore
   - `get_snapshot_builder` — returns self clone
   - `begin_receiving_snapshot` — returns empty cursor
   - `install_snapshot` — deserializes and restores SmState
   - `get_current_snapshot` — returns latest snapshot

2. **Command dispatch**
   ```bash
   cargo test -p syfrah-controlplane -- state_machine::tests::apply_create_org
   cargo test -p syfrah-controlplane -- state_machine::tests::apply_delete_org
   cargo test -p syfrah-controlplane -- state_machine::tests::apply_create_project
   ```
   Expected: commands dispatched to OrgStore, results match expectations

3. **Duplicate handling**
   ```bash
   cargo test -p syfrah-controlplane -- state_machine::tests::apply_create_org_duplicate
   ```
   Expected: returns Error response for duplicate creation

4. **Unimplemented commands**
   ```bash
   cargo test -p syfrah-controlplane -- state_machine::tests::apply_unimplemented_returns_ok
   ```
   Expected: returns Ok (not panic or error)

5. **Snapshot roundtrip**
   ```bash
   cargo test -p syfrah-controlplane -- state_machine::tests::snapshot_roundtrip
   ```
   Expected: snapshot can be built and retrieved

6. **Default applied state**
   ```bash
   cargo test -p syfrah-controlplane -- state_machine::tests::applied_state_default
   ```
   Expected: fresh state machine has no applied log and empty membership

## Pass criteria
- All tests pass
- Commands correctly modify OrgStore state
- Snapshot build/restore cycle works
