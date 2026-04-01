# E2E: Raft Snapshots — Periodic Compaction (#1069)

## Scope

Verify that Raft snapshots serialize the full redb state (org, IPAM,
placement tables) and that new members joining the cluster receive the
snapshot for fast catch-up instead of replaying the entire log.

## Prerequisites

- Two-node cluster with `controlplane init` + `controlplane join`
- An org, project, environment, subnet, and VM created via Raft

## Test Steps

### 1. Verify snapshot build includes store data

```bash
# On the leader node, check status to see log entries
ssh root@65.109.130.108 "syfrah controlplane status"
# Verify last_log_index grows as commands are applied
```

### 2. Verify snapshot configuration

```bash
# The snapshot_policy is set to LogsSinceLast(10000) by default
# For testing, this means snapshots trigger after 10,000 entries
# Verify the config is applied by checking Raft metrics
FABRIC_IP=$(ssh root@65.109.130.108 "ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print \$2}' | cut -d/ -f1 | head -1")
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7200/raft/status"
```

### 3. Verify new member catch-up via snapshot

```bash
# Create significant state on the leader
for i in $(seq 1 5); do
  ssh root@65.109.130.108 "syfrah org create snap-test-$i"
done

# On the second node, verify all orgs are visible
ssh root@37.27.12.205 "syfrah org list"
# All snap-test-{1..5} orgs should be present (replicated via Raft)
```

### 4. Verify snapshot survives restart

```bash
# Kill the leader daemon
ssh root@65.109.130.108 "kill $(cat ~/.syfrah/daemon.pid)"
sleep 5

# Restart
ssh root@65.109.130.108 "syfrah fabric start"
sleep 5

# Verify state is intact
ssh root@65.109.130.108 "syfrah controlplane status"
ssh root@65.109.130.108 "syfrah org list"
# All orgs should still exist
```

### 5. Verify log purge after snapshot

```bash
# After a snapshot, old log entries should be purged
# Check the log entry count before and after heavy operations
ssh root@65.109.130.108 "syfrah controlplane status --json" | python3 -c "
import json, sys
data = json.load(sys.stdin)
print(f'Log entries: {data.get(\"log_entries\", \"N/A\")}')
print(f'Last applied: {data.get(\"last_applied_index\", \"N/A\")}')
"
```

## Expected Results

1. Snapshots include full store table data (org, IPAM, placements)
2. `snapshot_policy` is `LogsSinceLast(10000)` by default
3. New members joining get the complete state via snapshot transfer
4. State survives daemon restart (persisted in redb)
5. Log entries are purged after snapshot, keeping only recent 100 entries

## Pass Criteria

- `syfrah controlplane status` shows correct state after restart
- All orgs/resources visible on both nodes after snapshot + catch-up
- No data loss after leader kill + restart
