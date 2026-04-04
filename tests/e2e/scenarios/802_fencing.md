# 802 — Validation: fencing correctness under concurrent writers

## Goal

Prove that volume fencing prevents split-brain under reschedule scenarios.
When a volume is moved from node A to node B via Raft generation bump, only the
node holding the current generation can write. The stale node must self-fence
(stop ZeroFS) and late writes from the stale generation must be invisible to the
new owner.

This is a GA gate test.

## Preconditions

- Two-node cluster (node A and node B) with Raft, Forge, and storage layer running
- S3-compatible object store reachable from both nodes
- ZeroFS binary available on both nodes

## Steps

### 1. Two-node cluster setup

Bootstrap a 2-node Raft cluster. Both nodes run Forge with the storage reconciler
enabled.

### 2. Create and attach volume on node A (gen=1)

```bash
syfrah storage volume create --name fence-test --size 10 --env env-test
syfrah storage volume attach fence-test --vm web-1 --hypervisor node-a
```

Verify: volume is `Attached`, `placement_generation=1`, `hypervisor_id=node-a`.

### 3. Write data on node A

Write a known payload through the NBD device on node A:

```bash
# On node A
echo "GENERATION_1_DATA" | dd of=/dev/nbdX bs=512 count=1
sync
```

Verify: data is flushed to S3 under the `gen-1/` prefix.

### 4. Simulate reschedule: Raft moves volume to node B (gen=2)

Submit a Raft command to reschedule the volume to node B. This bumps
`placement_generation` to 2 and sets `hypervisor_id` to `node-b`.

```bash
# Raft applies: volume.placement_generation = 2, hypervisor_id = node-b
syfrah storage volume attach fence-test --vm web-1 --hypervisor node-b
```

### 5. Node B starts ZeroFS with gen-2/ prefix

The storage reconciler on node B detects the new assignment, starts ZeroFS with
`s3_prefix = "volumes/{vol-id}/gen-2/"`. ZeroFS reads the committed manifest
from `gen-1/` to seed the LSM tree, then writes new data under `gen-2/`.

Verify: ZeroFS process running on node B with correct generation.

### 6. Verify: node B reads committed data from gen-1/ manifest

```bash
# On node B
dd if=/dev/nbdX bs=512 count=1 | grep "GENERATION_1_DATA"
```

Data written under gen-1 is visible to the gen-2 reader via the manifest chain.

### 7. Verify: node A's late writes (gen-1/) are invisible to node B

If node A's ZeroFS process has not yet been stopped (race window), any writes it
performs under `gen-1/` must not appear in node B's view. Node B's ZeroFS only
reads from its own generation's WAL, plus the sealed manifest from previous
generations.

Verify: write new data on node A (if still running), confirm it does NOT appear
when reading on node B.

### 8. Node A recovers: verify self-fencing

Node A's reconciler detects `local_generation (1) < raft_generation (2)` and
calls `VolumeMgr::self_fence_stale()`. This force-kills ZeroFS (no flush) and
discards the local cache.

```bash
# On node A — reconciler log
# Expected: "fenced volume fence-test: local gen 1 < raft gen 2"
```

Verify:
- ZeroFS process on node A is stopped (PID gone)
- NBD device on node A is disconnected
- No new data from node A appears in S3 under gen-1/ after fencing

### 9. Rapid reschedule: move 10 times, verify fencing each time

Loop 10 times:
1. Reschedule volume from current node to the other node (gen increments)
2. Wait for new node's ZeroFS to start
3. Verify old node self-fences within reconciliation timeout
4. Verify data continuity: all previously written data is readable
5. Write new data under current generation
6. Verify no split-brain: only one ZeroFS process active across both nodes

```
for gen in 3..12:
    move volume to alternate node
    assert: old node fenced within 30s
    assert: new node reads all prior data
    assert: exactly 1 ZeroFS process cluster-wide for this volume
```

## Expected Outcome

- **No split-brain**: at no point do two nodes run ZeroFS for the same volume
  with conflicting write paths.
- **Self-fencing works**: stale nodes detect generation mismatch and stop ZeroFS
  without flushing (avoiding corruption of the new generation's data).
- **Data continuity**: each new generation inherits all committed data from
  previous generations via the manifest chain.
- **Late writes invisible**: writes from a stale generation are not visible to
  the current generation's reader.
- **Rapid reschedule resilience**: even under 10 consecutive reschedules,
  fencing and data integrity hold.
