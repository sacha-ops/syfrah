# E2E 542 — CLI routing: transparent Raft forwarding from Forge

## Goal

Verify that `syfrah compute vm create` from any node routes through the
scheduler and creates VMs on the correct hypervisor, with `--zone` placing
on remote nodes transparently.

## Prerequisites

- Two-node cluster with Raft initialized and gossip running
- Both hypervisors registered with different zones (az-1, az-2)
- Org/project/env/subnet created

## Test steps

### 1. Create VM with --zone targeting remote hypervisor

```bash
# From the leader (node 1 in az-1), create VM targeting az-2 (node 2)
ssh root@65.109.130.108 "syfrah compute vm create --name zone-test \
  --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone az-2"
```

Expected: scheduler picks node 2 (az-2), creates VM there via remote Forge API.

### 2. Verify VM landed on correct node

```bash
ssh root@37.27.12.205 "syfrah compute vm list"
# zone-test should appear on node 2
ssh root@65.109.130.108 "syfrah compute vm list"
# zone-test should NOT appear on node 1
```

### 3. Create VM from follower (forwards to leader)

```bash
# From follower (node 2), create VM without zone constraint
ssh root@37.27.12.205 "syfrah compute vm create --name follower-test \
  --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg"
```

Expected: request forwarded to leader, scheduler picks a node, VM created.

### 4. Create VM without Raft (bootstrap mode)

If Raft is not initialized, `syfrah compute vm create` should create locally
as before (no scheduler, no forwarding).

### 5. Gossip is running

```bash
# Check that gossip agent is active (logs show gossip started)
ssh root@65.109.130.108 "grep 'gossip' ~/.syfrah/syfrah.log | tail -5"
ssh root@37.27.12.205 "grep 'gossip' ~/.syfrah/syfrah.log | tail -5"
```

Expected: gossip agent started and seeds announced.

### 6. Cleanup

```bash
ssh root@37.27.12.205 "syfrah compute vm delete zone-test --yes"
ssh root@65.109.130.108 "syfrah compute vm delete follower-test --yes"
```

## Pass criteria

- `--zone az-2` from leader creates VM on node 2 (not node 1)
- CLI from follower successfully creates VM (forwarded to leader)
- No Raft mode: local create works unchanged
- Gossip agent starts alongside daemon
- Scheduler logs show filter/score decision
