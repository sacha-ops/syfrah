# E2E: Control Plane Single-Node Bootstrap

**ID**: 506_cp_bootstrap
**Layer**: controlplane
**Priority**: P0

## Objective
Verify `syfrah controlplane init` bootstraps a single-node Raft cluster and that the daemon activates the Raft server on restart.

## Prerequisites
- Fabric initialized with `syfrah fabric init`
- Daemon running

## Steps

1. **Bootstrap**
   ```bash
   syfrah controlplane init
   ```
   Expected output:
   - Node ID, address displayed
   - State: Leader
   - Term: 1 or higher
   - Instructions to restart daemon

2. **Idempotent re-init**
   ```bash
   syfrah controlplane init
   ```
   Expected: "Control plane already initialized" message

3. **Daemon restart activates Raft**
   ```bash
   syfrah fabric stop && syfrah fabric start
   sleep 3
   curl -s http://[$FABRIC_IP]:7200/raft/status
   ```
   Expected: JSON status with leader, term, members

4. **Backward compatibility**
   After init, verify existing commands still work:
   ```bash
   syfrah org create testorg
   syfrah org list
   syfrah org delete testorg --yes
   ```

## Pass criteria
- Single-node cluster bootstraps successfully
- Node becomes leader
- Raft server responds after daemon restart
- Existing CLI commands unaffected
