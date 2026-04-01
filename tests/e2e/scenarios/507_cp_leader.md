# E2E: Control Plane Leader Election Verification

**ID**: 507_cp_leader
**Layer**: controlplane
**Priority**: P0

## Objective
Verify that after single-node bootstrap, the node becomes leader automatically, and the Raft state is visible in both `controlplane status` and the Forge health endpoint.

## Prerequisites
- Fabric initialized and daemon running
- `syfrah controlplane init` completed

## Steps

1. **Init and verify leader**
   ```bash
   syfrah controlplane init
   ```
   Expected: state = Leader, term >= 1

2. **Status shows leader**
   ```bash
   syfrah fabric stop && syfrah fabric start
   sleep 3
   syfrah controlplane status
   ```
   Expected:
   - State: Leader
   - Leader: matches own node ID
   - Term: >= 1
   - Members: exactly 1 member

3. **JSON status**
   ```bash
   syfrah controlplane status --json
   ```
   Expected: valid JSON with id, state, current_leader, current_term, members

4. **Forge health includes Raft info**
   ```bash
   curl -s http://[$FABRIC_IP]:7100/v1/hypervisor/health | python3 -m json.tool
   ```
   Expected: response includes `raft` field with:
   - `initialized: true`
   - `state: "Leader"`
   - `is_leader: true`
   - `term: >= 1`

5. **Verify existing features still work**
   ```bash
   syfrah org create acme
   syfrah project create backend --org acme
   syfrah org list
   ```
   Expected: all commands succeed (backward compatible)

## Pass criteria
- Single node is leader after init
- Status command shows correct Raft state
- Forge health endpoint includes Raft info
- Existing CLI commands work unchanged
