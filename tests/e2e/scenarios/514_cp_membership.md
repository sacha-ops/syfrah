# 514 — Control Plane: Multi-node Raft cluster — add/remove members

## Goal
Verify that a second node can join the Raft cluster and both nodes share state.

## Steps

### 1. Bootstrap Raft on node 1
```bash
ssh root@node1 "syfrah controlplane init"
```

### 2. Join node 2
```bash
ssh root@node2 "syfrah controlplane join"
# Should find the leader on node 1, send join request, write sentinel
```

### 3. Restart node 2 daemon
```bash
ssh root@node2 "syfrah fabric stop && syfrah fabric start"
```

### 4. Verify membership
```bash
ssh root@node1 "syfrah controlplane members"
# Should show 2 members
ssh root@node2 "syfrah controlplane members"
```

### 5. Create data on node 1, verify on node 2
```bash
ssh root@node1 "syfrah org create test-replication"
sleep 2
ssh root@node2 "syfrah org list"
# Should show test-replication (replicated via Raft)
```

## Expected Outcome
- `syfrah controlplane join` adds node as learner to the Raft cluster.
- `syfrah controlplane members` shows all members with roles (voter/learner).
- State created on any node is replicated to all nodes.
