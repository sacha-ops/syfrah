# 560 — Auto-join Raft on fabric join

## Purpose

Verify that a node joining the fabric mesh automatically detects and joins an
existing Raft control plane cluster without requiring a separate
`syfrah controlplane join` command.

## Prerequisites

- 3 servers with `syfrah` installed (S1, S2, S3)
- No prior mesh or control plane state on any server

## Steps

### 1. Initialize mesh and control plane on S1

```bash
ssh root@$S1 "syfrah fabric init --name cloud --node-name hv-1 --endpoint $S1:51820 --region eu --zone fsn1"
ssh root@$S1 "syfrah controlplane init && syfrah fabric stop && sleep 2 && syfrah fabric start"
sleep 5
ssh root@$S1 "syfrah fabric peering start --pin 4829"
```

### 2. Join S2 to fabric (auto-join should trigger)

```bash
ssh root@$S2 "syfrah fabric join $S1 --pin 4829 --node-name hv-2 --endpoint $S2:51820 --region eu --zone nbg1"
sleep 15  # wait for auto-join (5s delay + probe + join + restart)
```

**Expected:** S2 daemon logs show:
- `raft: detected Raft cluster (leader: ...)`
- `raft: auto-join succeeded!`

### 3. Verify S2 is a Raft member

```bash
ssh root@$S2 "syfrah controlplane status"
```

**Expected:** Shows `Raft: Learner` or `Voter` (after auto-promote).

```bash
ssh root@$S1 "syfrah controlplane members"
```

**Expected:** Shows 2 members (hv-1 and hv-2).

### 4. Verify fabric status shows control plane

```bash
ssh root@$S2 "syfrah fabric status"
```

**Expected:** Shows `Control Plane: joined (learner, term N)` or
`Control Plane: joined (voter, term N)`.

### 5. Join S3 to fabric (auto-join should trigger)

```bash
ssh root@$S3 "syfrah fabric join $S1 --pin 4829 --node-name hv-3 --endpoint $S3:51820 --region eu --zone hel1"
sleep 15
ssh root@$S1 "syfrah controlplane members"
```

**Expected:** Shows 3 members.

### 6. Test data replication

```bash
ssh root@$S1 "syfrah org create test-org"
sleep 3
ssh root@$S3 "syfrah org list"
```

**Expected:** `test-org` is visible on S3.

### 7. Edge case: first node without Raft stays in bootstrap mode

```bash
# On a fresh node:
syfrah fabric init --name solo --node-name lonely
syfrah fabric status
```

**Expected:** Shows `Control Plane: not available (bootstrap mode)`.

### 8. Edge case: node with existing raft_initialized skips auto-join

```bash
# On S2 (already joined):
syfrah fabric stop && syfrah fabric start
sleep 10
syfrah controlplane status
```

**Expected:** Raft starts normally from stored state. No auto-join triggered.

## Pass criteria

- [ ] S2 and S3 auto-join Raft within 20s of fabric join
- [ ] `fabric status` shows control plane status on all nodes
- [ ] First node (no peers) stays in bootstrap mode
- [ ] Node restart with existing sentinel does not re-trigger auto-join
- [ ] Data replicated from leader is visible on all members
