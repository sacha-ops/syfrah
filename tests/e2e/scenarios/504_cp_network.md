# E2E: Control Plane Raft Network

**ID**: 504_cp_network
**Layer**: controlplane
**Priority**: P0

## Objective
Verify the Raft network implementation correctly sends RPCs over HTTP/JSON to remote nodes.

## Prerequisites
- Control plane crate builds

## Steps

1. **RaftNetworkFactory implemented**
   Verify `SyfrahNetworkFactory` implements `RaftNetworkFactory<SyfrahRaftConfig>`:
   - `new_client` creates a `SyfrahNetwork` for the target node

2. **RaftNetworkV2 implemented**
   Verify `SyfrahNetwork` implements `RaftNetworkV2<SyfrahRaftConfig>`:
   - `append_entries` — POST to `/raft/append_entries`
   - `vote` — POST to `/raft/vote`
   - `full_snapshot` — POST to `/raft/install_snapshot`

3. **Network factory construction**
   ```bash
   cargo test -p syfrah-controlplane -- network
   ```
   Expected: factory creates clients without error

4. **Compile check**
   ```bash
   cargo build -p syfrah-controlplane
   ```
   Expected: clean compilation

## Pass criteria
- Network types implement required openraft traits
- HTTP endpoints match server routes
- Reqwest client configured with 5s timeout
