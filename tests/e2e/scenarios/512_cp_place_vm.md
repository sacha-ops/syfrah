# 512 — Control Plane: PlaceVm command — scheduler commits placement to Raft

## Goal
Verify that VM placements are recorded in Raft via the PlaceVm command, making placement data available on all cluster nodes.

## Preconditions
- Two-node Raft cluster
- Org/project/env/subnet hierarchy created

## Steps

### 1. Create a VM (triggers PlaceVm through Raft)
```bash
syfrah compute vm create --name web-1 --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub
```

### 2. Verify placement is recorded
```bash
# The placement record should exist on both nodes
ssh root@node1 "syfrah compute vm get web-1 --json"
ssh root@node2 "syfrah compute vm get web-1 --json"
# Both should show the same hypervisor_id and placement_generation
```

### 3. Delete VM (triggers RemoveVm through Raft)
```bash
syfrah compute vm delete web-1 --yes
```

## Expected Outcome
- PlaceVm writes `VmPlacement` record with generation to the placement store via Raft.
- All nodes see the same placement data (replicated through Raft state machine).
- RemoveVm cleans up the placement record on all nodes.
- VmPlacement now includes `placement_generation` field for fencing support.
