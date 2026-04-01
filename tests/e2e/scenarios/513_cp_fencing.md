# 513 — Control Plane: Placement fencing — generation-based double-run protection

## Goal
Verify that when a VM is rescheduled from node A to node B, node A detects the stale placement and fences (stops) the VM locally.

## Preconditions
- Two-node Raft cluster with VMs running

## Steps

### 1. Create a VM on node 1
```bash
ssh root@node1 "syfrah compute vm create --name web-1 ..."
```

### 2. Simulate reschedule by updating placement in Raft
```bash
# Increment placement_generation and change hypervisor_id to node-2
```

### 3. On node 1's next reconciliation cycle
```
FencingTracker.check("web-1", new_generation, "node-2")
→ FencingVerdict::Fenced { reason: "rescheduled: local gen 1 < raft gen 2" }
```

### 4. Node 1 should stop the VM and clean up local resources

## Expected Outcome
- FencingTracker correctly identifies stale placements.
- VMs are fenced when their generation is behind or hypervisor_id mismatches.
- No double-running: only one node runs each VM at any time.
