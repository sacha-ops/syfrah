# 281 - VM Delete Network Cleanup

## Objective

Verify that `vm delete` correctly tears down all network resources associated with the VM.

## Prerequisites

- A running Syfrah node with overlay networking configured
- An org, project, environment, and subnet already created
- A VM created with networking (has an IP, TAP, FDB entry, nftables rules)

## Steps

### 1. Create a VM with networking

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
syfrah subnet create frontend --env production --project backend --org acme
syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048
```

Expected: VM is created with an IP (e.g., 10.0.1.3), TAP device, FDB entry, and nftables rules.

### 2. Verify network resources exist

```bash
# TAP exists
ip link show syftap-web-1

# Bridge has gateway IP
ip addr show syfbr-$(VPC_ID) | grep "10.0.1.1"

# nftables rules exist for the TAP
nft list chain bridge syfrah syftap-web-1-in 2>/dev/null
nft list chain bridge syfrah syftap-web-1-out 2>/dev/null
```

### 3. Delete the VM

```bash
syfrah compute vm delete --name web-1 --project backend --org acme
```

Expected: VM is deleted successfully. Output confirms deletion.

### 4. Verify network resources are cleaned up

```bash
# TAP is gone
ip link show syftap-web-1 2>&1 | grep -q "does not exist"

# FDB entry is gone (no entry for the VM's MAC on the bridge)
bridge fdb show dev syfvx-$(VPC_ID) | grep -v "02:00:0a:00:01:03"

# nftables rules are gone
nft list chain bridge syfrah syftap-web-1-in 2>&1 | grep -q "No such"
nft list chain bridge syfrah syftap-web-1-out 2>&1 | grep -q "No such"

# IP is released (can be re-allocated)
# Create a new VM and verify it gets the same IP
syfrah compute vm create --name web-2 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048
# Expected: web-2 gets 10.0.1.3 (the released IP)
```

### 5. Verify bridge cleanup when last VM is deleted

```bash
syfrah compute vm delete --name web-2 --project backend --org acme

# Bridge should be removed (no more VMs in this VPC on this node)
ip link show syfbr-$(VPC_ID) 2>&1 | grep -q "does not exist"

# VXLAN should be removed
ip link show syfvx-$(VPC_ID) 2>&1 | grep -q "does not exist"

# NAT rules for this subnet should be gone
nft list ruleset | grep -v "10.0.1.0/24"
```

### 6. Verify bridge kept when other VMs exist

```bash
# Create two VMs
syfrah compute vm create --name web-a --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048
syfrah compute vm create --name web-b --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048

# Delete one
syfrah compute vm delete --name web-a --project backend --org acme

# Bridge should still exist (web-b is still running)
ip link show syfbr-$(VPC_ID)

# Clean up
syfrah compute vm delete --name web-b --project backend --org acme
```

## Pass criteria

- TAP device is removed on VM delete
- FDB entry is removed on VM delete
- IP is released back to IPAM on VM delete
- nftables per-TAP chains are removed on VM delete
- Bridge + VXLAN + NAT are removed when last VM on the bridge is deleted
- Bridge + VXLAN + NAT are kept when other VMs still use the bridge
- All cleanup is best-effort: partial failures do not block the delete
