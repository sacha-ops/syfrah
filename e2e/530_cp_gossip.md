# E2E 530 — Control Plane Gossip (SWIM via foca)

## Objective
Verify that the SWIM gossip protocol (foca crate) is integrated into the
control plane and can track membership states (Alive/Suspect/Down) and
disseminate HypervisorGossipReport data.

## Prerequisites
- Two-node fabric mesh (hv-eu-1 + hv-eu-2)
- Control plane initialized (Raft leader + follower)
- Hypervisors registered and enabled

## Test Steps

### 1. Gossip module compiles and unit tests pass
```bash
cargo test -p syfrah-controlplane -- gossip
```
Expected: all gossip tests pass.

### 2. GossipCluster stores and retrieves reports
- Create a `GossipCluster`, insert a `HypervisorGossipReport`
- Verify `all_reports()` returns the report
- Verify `get_report(node_name)` returns the correct report
- Verify utilization calculations (cpu_utilization, memory_utilization)

### 3. Member state transitions
- Set member state to Alive, verify `get_member_state` returns Alive
- Set member state to Down, verify `down_members()` includes the member
- Use `mark_down(node_name)` to mark a member down by name

### 4. GossipNodeId identity contract
- Verify `renew()` increments the bump counter
- Verify `win_addr_conflict()` prefers higher bump
- Verify `addr()` returns the socket address

### 5. Foca integration (runtime)
- Start gossip agent on localhost for testing
- Verify it binds to the configured UDP port
- Verify it publishes an initial local report to the cluster

## Pass Criteria
- All unit tests in `gossip.rs` pass
- `GossipCluster` correctly stores/retrieves reports and member states
- `HypervisorGossipReport` utilization helpers work correctly
- Module compiles cleanly with no clippy warnings
