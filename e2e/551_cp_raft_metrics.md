# E2E: Raft Metrics — Prometheus Integration (#1070)

## Scope

Verify that Raft-specific metrics are exposed on the `/metrics` endpoint
(port 7100) in Prometheus text exposition format.

## Prerequisites

- Two-node cluster with `controlplane init` + `controlplane join`
- Daemon running on both nodes

## Test Steps

### 1. Query metrics endpoint for Raft metrics

```bash
FABRIC_IP=$(ssh root@65.109.130.108 "ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print \$2}' | cut -d/ -f1 | head -1")
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep raft"
```

### 2. Verify all expected Raft metrics are present

```bash
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics" | grep -E "^raft_"
# Expected output:
# raft_state 2          (leader = 2)
# raft_term <N>
# raft_commit_index <N>
# raft_last_applied <N>
# raft_log_entries <N>
# raft_snapshot_count <N>
```

### 3. Verify follower metrics differ

```bash
FABRIC_IP2=$(ssh root@37.27.12.205 "ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print \$2}' | cut -d/ -f1 | head -1")
ssh root@37.27.12.205 "curl -s http://[$FABRIC_IP2]:7100/metrics | grep raft"
# raft_state should be 0 (follower)
```

### 4. Verify metrics update after operations

```bash
# Create an org (triggers Raft write)
ssh root@65.109.130.108 "syfrah org create metrics-test"

# Check that commit_index/last_applied increased
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep raft_commit_index"
```

### 5. Verify HELP and TYPE annotations

```bash
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep '# HELP raft'"
# Should see HELP lines for all 6 raft metrics
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep '# TYPE raft'"
# Should see TYPE lines (gauge for most, counter for snapshot_count)
```

## Expected Metrics

| Metric              | Type    | Description                                   |
|---------------------|---------|-----------------------------------------------|
| raft_state          | gauge   | 0=follower, 1=candidate, 2=leader             |
| raft_term           | gauge   | Current Raft term                              |
| raft_commit_index   | gauge   | Raft commit index                              |
| raft_last_applied   | gauge   | Index of last applied log entry                |
| raft_log_entries    | gauge   | Current number of log entries                  |
| raft_snapshot_count | counter | Number of snapshots taken                      |

## Pass Criteria

- All 6 raft metrics present in `/metrics` output
- Leader shows `raft_state 2`, follower shows `raft_state 0`
- Metrics update after Raft write operations
- Prometheus HELP/TYPE annotations present for all metrics
