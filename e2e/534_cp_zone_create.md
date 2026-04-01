# E2E 534 — vm create --zone end-to-end

## Objective
Verify the full flow: `syfrah compute vm create --zone az-2` places
the VM on the correct hypervisor via the scheduler.

## Flow
```
syfrah compute vm create --name web-3 --zone az-2 ...
-> Forge on this node receives request
-> If Raft leader: run scheduler
-> If follower: forward to leader
-> Scheduler filters by zone=az-2, picks hv-eu-2
-> Calls target Forge's HTTP API to create the VM
-> Records PlaceVm in Raft
-> FDB entries distributed to all nodes
-> Response returned to caller with VM IP
```

## Prerequisites
- Two-node fabric mesh (hv-eu-1 in az-1, hv-eu-2 in az-2)
- Raft initialized with both nodes
- Hypervisors registered and enabled
- Org hierarchy created (org, project, env, subnet, NAT GW, SG)

## Test Steps

### 1. Remote create module works
- `RemoteCreateVmRequest` serializes/deserializes correctly
- `forge_addr_from_fabric_ipv6` formats addresses correctly

### 2. Create VM with --zone from server 1
```bash
ssh root@65.109.130.108 "syfrah compute vm create --name remote-vm \
  --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone az-2"
```

### 3. Verify VM is on server 2
```bash
ssh root@37.27.12.205 "syfrah compute vm list"
ssh root@37.27.12.205 "syfrah compute vm get remote-vm"
```

### 4. Cross-node network connectivity
- Create local VM on server 1
- Ping between VMs across VXLAN

### 5. Cleanup
- Delete both VMs

## Pass Criteria
- VM created with `--zone az-2` is placed on hv-eu-2
- VM is accessible via SSH from the target node
- Cross-node VXLAN ping works
- PlaceVm recorded in Raft
