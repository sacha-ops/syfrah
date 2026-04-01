# 511 — Control Plane: Distributed IPAM through Raft

## Goal
Verify that IP allocation and release go through Raft, ensuring cluster-wide uniqueness. Two nodes sharing the same Raft cluster must never allocate duplicate IPs.

## Preconditions
- Two-node mesh initialized
- Control plane bootstrapped on node 1, node 2 joined
- Both nodes share the same Raft cluster

## Steps

### 1. Create subnet
```bash
# On node 1 (leader)
syfrah org create acme
syfrah project create backend --org acme
syfrah env create prod --project backend --org acme
syfrah subnet create web --env prod --project backend --org acme
```

### 2. Create VM on node 1 — triggers AllocateIp through Raft
```bash
ssh root@node1 "syfrah compute vm create --name vm-1 --image alpine-3.20 --vcpus 1 --memory 512 --env prod --subnet web --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub"
```

### 3. Create VM on node 2 — AllocateIp goes through Raft to the same bitmap
```bash
ssh root@node2 "syfrah compute vm create --name vm-2 --image alpine-3.20 --vcpus 1 --memory 512 --env prod --subnet web --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub"
```

### 4. Verify unique IPs
```bash
# Both VMs must have DIFFERENT IPs (e.g., 10.1.0.3 and 10.1.0.4)
ssh root@node1 "syfrah compute vm get vm-1 --json"
ssh root@node2 "syfrah compute vm get vm-2 --json"
```

### 5. Delete VM and verify IP release
```bash
ssh root@node1 "syfrah compute vm delete vm-1 --yes"
# The released IP should be available for future allocations
```

## Expected Outcome
- AllocateIp and ReleaseIp commands go through Raft as `StateMachineCommand`.
- The IPAM bitmap is maintained in the Raft state machine (same redb on all nodes).
- No duplicate IPs across the cluster.
- The state machine returns `AllocatedIp { ip, mac }` on success.
