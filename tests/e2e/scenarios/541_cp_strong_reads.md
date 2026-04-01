# E2E 541 — Strong reads via ?consistency=strong

## Goal

Verify that GET endpoints with `?consistency=strong` query parameter
forward reads to the Raft leader for linearizable (guaranteed up-to-date)
results, while default reads serve from local (eventually consistent) state.

## Prerequisites

- Two-node cluster with Raft initialized
- At least one VM created

## Test steps

### 1. Default read (eventually consistent)

```bash
# Read from follower — returns local data
ssh root@37.27.12.205 "curl -s http://[\$FABRIC_IPV6]:7100/v1/instances"
```

Expected: returns local instance list (may be slightly stale).

### 2. Strong read from follower

```bash
# Read from follower with strong consistency — forwarded to leader
ssh root@37.27.12.205 "curl -s 'http://[\$FABRIC_IPV6]:7100/v1/instances?consistency=strong'"
```

Expected: returns leader's instance list (guaranteed up-to-date).

### 3. Strong read of specific instance

```bash
ssh root@37.27.12.205 "curl -s 'http://[\$FABRIC_IPV6]:7100/v1/instances/test-vm?consistency=strong'"
```

Expected: returns VM details from leader.

### 4. Strong read from leader itself

```bash
# On the leader, strong read should serve locally (no forwarding)
ssh root@65.109.130.108 "curl -s 'http://[\$FABRIC_IPV6]:7100/v1/instances?consistency=strong'"
```

Expected: returns local data (leader is authoritative).

### 5. Default read is fast (no forwarding)

```bash
# Time default vs strong reads
ssh root@37.27.12.205 "time curl -s http://[\$FABRIC_IPV6]:7100/v1/instances > /dev/null"
ssh root@37.27.12.205 "time curl -s 'http://[\$FABRIC_IPV6]:7100/v1/instances?consistency=strong' > /dev/null"
```

Expected: default read is faster (no network hop to leader).

## Pass criteria

- Default GET returns local data without contacting the leader
- `?consistency=strong` on a follower forwards to the leader and returns data
- `?consistency=strong` on the leader serves locally
- Unknown consistency values are treated as default (no error)
