# E2E 543 — Enhanced cluster status CLI

## Goal

Verify `syfrah controlplane status` shows Raft, Members, and log details.

## Prerequisites

- Two-node cluster with Raft initialized

## Test steps

### 1. Status on leader

```bash
ssh root@65.109.130.108 "syfrah controlplane status"
```

Expected output format:
```
Control Plane Status
====================
  Raft:       Leader (term 3, commit 47)
  Members:    2 (1 voter, 1 learner)
  Log:        47 entries

Members:
  hv-eu-1        voter    Leader     fd12::...
  hv-eu-2        learner  Learner    fd12::...

Node ID:      12345678
```

### 2. Status on follower

```bash
ssh root@37.27.12.205 "syfrah controlplane status"
```

Expected: shows Follower role, same members list.

### 3. JSON output

```bash
ssh root@65.109.130.108 "syfrah controlplane status --json"
```

Expected: full JSON with `member_details`, `voter_count`, `learner_count`, `commit_index`.

## Pass criteria

- Status shows Leader/Follower role correctly
- Members table shows all cluster nodes with roles
- Term, commit index, and log entries displayed
- JSON output includes all enhanced fields
- Node names resolved from fabric state (not raw IDs)
