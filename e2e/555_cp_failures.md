# E2E: Failure Scenario Tests (#1074)

## Scope

Comprehensive test plans for Raft and gossip failure scenarios. Some tests
are executable on the test servers; others are documented for future automation.

## Prerequisites

- Two-node cluster: hv-eu-1 (65.109.130.108) and hv-eu-2 (37.27.12.205)
- Control plane initialized with both nodes as voters
- Some data created (org, VPC, subnet, VMs)

---

## Scenario 1: Leader Crash — Automatic Re-election

**Goal:** Verify that if the leader process dies, the remaining voter
elects a new leader automatically.

### Steps

```bash
# 1. Identify the current leader
ssh root@65.109.130.108 "syfrah controlplane status"
# Note which node is Leader

# 2. Kill the leader process
ssh root@65.109.130.108 "kill $(cat ~/.syfrah/daemon.pid)"

# 3. Wait for re-election (typically 1-3 seconds)
sleep 10

# 4. Check status from the surviving node
ssh root@37.27.12.205 "syfrah controlplane status"
# Expected: hv-eu-2 is now Leader, term incremented

# 5. Verify writes work on the new leader
ssh root@37.27.12.205 "syfrah org create failover-test"

# 6. Restart the old leader
ssh root@65.109.130.108 "syfrah fabric start"
sleep 5

# 7. Verify state is consistent
ssh root@65.109.130.108 "syfrah controlplane status"
ssh root@65.109.130.108 "syfrah org list"
# failover-test should be visible (replicated from new leader)
```

### Expected Results

- New leader elected within 5 seconds
- Term number increases
- Writes succeed on the new leader
- Restarted node rejoins and catches up

---

## Scenario 2: Network Partition — Minority Side Cannot Write

**Goal:** Verify that the minority side of a partition returns 503 on
writes, while reads may still work (eventual consistency).

### Steps

```bash
# 1. Simulate partition by blocking Raft traffic on one node
# (block port 7200 which is used for Raft RPCs)
ssh root@37.27.12.205 "nft add table inet raft_block && \
  nft add chain inet raft_block input '{ type filter hook input priority 0; }' && \
  nft add rule inet raft_block input tcp dport 7200 drop && \
  nft add rule inet raft_block input tcp sport 7200 drop"

# 2. Wait for partition detection
sleep 15

# 3. Try write on the partitioned node (should fail with 503)
ssh root@37.27.12.205 "syfrah org create partition-test 2>&1"
# Expected: error / 503 (no leader reachable)

# 4. Write on the majority side (should succeed)
ssh root@65.109.130.108 "syfrah org create partition-ok"

# 5. Remove the partition
ssh root@37.27.12.205 "nft delete table inet raft_block"

# 6. Wait for reconnection
sleep 10

# 7. Verify state converges
ssh root@37.27.12.205 "syfrah org list"
# partition-ok should appear (replicated after heal)
# partition-test should NOT exist (write was rejected)
```

### Expected Results

- Partitioned minority node cannot write (503 or error)
- Majority side continues normal operation
- After partition heals, state converges

### Note

In a 2-node cluster, neither side has a majority. This means BOTH sides
will lose the leader. True partition testing requires 3+ nodes. With
2 nodes, killing one is effectively the same as a partition.

---

## Scenario 3: Node Crash — Raft Log Replayed on Restart

**Goal:** Verify that a crashed node recovers its state from the
persisted Raft log on restart.

### Steps

```bash
# 1. Create some state
ssh root@65.109.130.108 "syfrah org create crash-test"

# 2. Kill the daemon (simulating a crash)
ssh root@65.109.130.108 "kill -9 $(cat ~/.syfrah/daemon.pid)"

# 3. Verify the process is dead
ssh root@65.109.130.108 "pgrep syfrah" || echo "Process killed"

# 4. Restart
ssh root@65.109.130.108 "syfrah fabric start"
sleep 5

# 5. Verify state is recovered
ssh root@65.109.130.108 "syfrah controlplane status"
ssh root@65.109.130.108 "syfrah org list"
# crash-test should still exist (recovered from Raft log)

# 6. Verify the node rejoins the cluster
ssh root@65.109.130.108 "syfrah controlplane members"
# Both nodes should be listed
```

### Expected Results

- Raft log entries persisted in redb survive the crash
- On restart, the state machine replays the log and recovers state
- Node rejoins the cluster and resumes normal operation
- All previously created resources are intact

---

## Scenario 4: Stale Reads on Follower

**Goal:** Verify that reads on a follower may lag behind the leader
(eventual consistency).

### Steps

```bash
# 1. Create an org on the leader
ssh root@65.109.130.108 "syfrah org create stale-test"

# 2. Immediately read on the follower
ssh root@37.27.12.205 "syfrah org list"
# stale-test may or may not appear (depends on replication lag)

# 3. Wait for replication
sleep 2

# 4. Read again
ssh root@37.27.12.205 "syfrah org list"
# stale-test should now appear
```

### Expected Results

- Writes only go through the leader
- Follower reads may lag by a few hundred milliseconds
- After a short delay, follower state catches up

### Note

For strong consistency, the Forge API supports `?consistency=strong`
which routes reads through the leader.

---

## Scenario 5: Gossip Partition — Nodes Appear Suspect/Down

**Goal:** Verify that gossip and Raft are independent: gossip partition
does not prevent Raft from functioning as long as Raft network is intact.

### Steps

```bash
# 1. Block gossip traffic (port 7300 UDP) but leave Raft (7200 TCP) open
ssh root@37.27.12.205 "nft add table inet gossip_block && \
  nft add chain inet gossip_block input '{ type filter hook input priority 0; }' && \
  nft add rule inet gossip_block input udp dport 7300 drop && \
  nft add rule inet gossip_block input udp sport 7300 drop"

# 2. Wait for gossip to detect the partition
sleep 30

# 3. Check gossip state — node should appear suspect or down
FABRIC_IP=$(ssh root@65.109.130.108 "ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print \$2}' | cut -d/ -f1 | head -1")
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep gossip"
# gossip_members_total{state="suspect"} or {state="down"} should show 1

# 4. Verify Raft still works
ssh root@65.109.130.108 "syfrah controlplane status"
# Should still show 2 voters, leader elected
ssh root@65.109.130.108 "syfrah org create gossip-partition-test"
# Write should succeed (Raft is unaffected)

# 5. Remove gossip partition
ssh root@37.27.12.205 "nft delete table inet gossip_block"
sleep 10

# 6. Verify gossip recovers
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep gossip"
# Members should return to alive state
```

### Expected Results

- Gossip partition causes nodes to appear suspect/down in gossip
- Raft consensus continues working (different network path)
- After gossip partition heals, members return to alive state
- Data created during partition is consistent on both nodes

---

## Summary Matrix

| Scenario          | Executable on Test Servers | Automation Status |
|-------------------|---------------------------|-------------------|
| Leader crash      | Yes                       | Manual            |
| Network partition | Partial (2-node limit)    | Manual            |
| Node crash        | Yes                       | Manual            |
| Stale reads       | Yes                       | Manual            |
| Gossip partition  | Yes (nft rules)           | Manual            |

All scenarios should be run after deploying a new build to both test
servers. The leader crash and node crash scenarios are safe to run
repeatedly. Partition scenarios require nftables cleanup.
