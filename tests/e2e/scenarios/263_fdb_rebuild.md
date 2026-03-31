# Test: FDB tables rebuilt on daemon restart

## Objective

- After a daemon restart, FDB and ARP proxy entries are re-populated from the persisted `vm_placements` table
- Local placements are skipped (no FDB entry needed)
- Remote placements get both FDB and ARP proxy entries
- Stale/unreachable placements are counted as errors but do not block startup

## Prerequisites

- 2 test servers with `syfrah` installed and in PATH
- A working mesh between the two nodes
- At least one VM created on each node within a shared VPC
- The VM placements are persisted in redb (`vm_placements` table)

## Steps

### 1. Create VMs across both nodes

On node-1:
```bash
syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 1 --memory 512
```

On node-2:
```bash
syfrah compute vm create --name web-2 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 1 --memory 512
```

Verify cross-node connectivity:
```bash
# From node-1
ssh root@<web-1-ip> "ping -c 3 <web-2-ip>"
```

### 2. Record the FDB state before restart

On node-1:
```bash
bridge fdb show dev syfvx-$(syfrah vpc list --project backend --org acme -o json | jq -r '.[0].id')
```

Save the output for comparison.

### 3. Restart the daemon on node-1

```bash
syfrah fabric stop
syfrah fabric start
```

Wait for the daemon to complete startup (timeout: 30s).

### 4. Verify FDB entries are rebuilt

On node-1:
```bash
bridge fdb show dev syfvx-$(syfrah vpc list --project backend --org acme -o json | jq -r '.[0].id')
```

**Expected**: the FDB entry for web-2's MAC pointing to node-2's fabric IPv6 is present.

### 5. Verify ARP proxy entries are rebuilt

On node-1:
```bash
ip neigh show dev syfvx-$(syfrah vpc list --project backend --org acme -o json | jq -r '.[0].id')
```

**Expected**: a permanent neighbor entry for web-2's IP/MAC is present.

### 6. Verify cross-node connectivity after rebuild

```bash
ssh root@<web-1-ip> "ping -c 3 <web-2-ip>"
```

**Expected**: ping succeeds without packet loss.

## Pass criteria

- [ ] Daemon restarts without errors
- [ ] FDB entries match the pre-restart state
- [ ] ARP proxy entries match the pre-restart state
- [ ] Cross-node VM connectivity works immediately after restart
- [ ] Local VM placements do not produce FDB entries (no self-referencing VTEP)
