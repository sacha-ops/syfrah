# E2E: Control Plane Raft HTTP Server

**ID**: 505_cp_raft_server
**Layer**: controlplane
**Priority**: P0

## Objective
Verify the Raft HTTP server exposes correct routes and is wired into daemon startup.

## Steps

1. **Server routes defined**
   Verify Axum router includes:
   - POST `/raft/append_entries`
   - POST `/raft/vote`
   - POST `/raft/install_snapshot`
   - GET `/raft/status`

2. **Wired into daemon**
   Verify daemon.rs starts the Raft server on syfrah0:7200 when control plane is initialized.

3. **Build check**
   ```bash
   cargo build --workspace
   ```
   Expected: clean compilation

4. **Status endpoint**
   After `syfrah controlplane init`, verify:
   ```bash
   curl -s http://[$FABRIC_IP]:7200/raft/status
   ```
   Returns JSON with: id, state, current_leader, current_term, last_log_index, members

## Pass criteria
- Server starts alongside Forge on daemon startup
- Routes match network client endpoints
- Status endpoint returns valid JSON
