# E2E 540 — Any-node API access (transparent leader forwarding)

## Goal

Verify that Forge HTTP API mutation requests (POST/DELETE `/v1/instances`)
on a follower node are transparently forwarded to the Raft leader.

## Prerequisites

- Two-node cluster with Raft initialized (leader on node 1, follower on node 2)
- Fabric mesh and hypervisors registered

## Test steps

### 1. Identify leader and follower

```bash
ssh root@65.109.130.108 "syfrah controlplane status"
ssh root@37.27.12.205 "syfrah controlplane status"
```

One node should report `Leader`, the other `Follower`.

### 2. Create instance via follower's Forge API

```bash
# Assuming node 2 is follower — get its fabric IPv6
FOLLOWER_IPV6=$(ssh root@37.27.12.205 "syfrah fabric status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)[\"mesh_ipv6\"])'")

# POST to follower's Forge API directly
ssh root@37.27.12.205 "curl -s -X POST http://[$FOLLOWER_IPV6]:7100/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{\"name\":\"forward-test\",\"image\":\"alpine-3.20\",\"vcpus\":1,\"memory_mb\":512}'"
```

Expected: request is forwarded to leader, VM is created (possibly on leader's hypervisor).

### 3. Verify VM exists via leader

```bash
ssh root@65.109.130.108 "syfrah compute vm list"
```

Expected: `forward-test` appears in the list.

### 4. Read from follower (no forwarding)

```bash
ssh root@37.27.12.205 "curl -s http://[$FOLLOWER_IPV6]:7100/v1/instances"
```

Expected: returns local instance list (served from local state, no forwarding).

### 5. Delete via follower

```bash
ssh root@37.27.12.205 "curl -s -X DELETE http://[$FOLLOWER_IPV6]:7100/v1/instances/forward-test"
```

Expected: forwarded to leader, VM is deleted.

### 6. Cleanup verification

```bash
ssh root@65.109.130.108 "syfrah compute vm list"
```

Expected: `forward-test` no longer appears.

## Pass criteria

- POST to follower returns 201 (created) — the response came from the leader
- GET on follower returns local data (no forwarding)
- DELETE on follower forwards to leader and succeeds
- No errors in daemon logs about forwarding failures
