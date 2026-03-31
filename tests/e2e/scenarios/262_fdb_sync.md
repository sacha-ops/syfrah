# Test: FDB sync on VM placement announcement

## Objective

Verify that receiving a `VmPlacement` announcement correctly updates the local FDB and ARP proxy tables on the node's VPC bridge and VXLAN interface.

## Prerequisites

- Two test servers joined in the same fabric mesh
- A VPC with at least one subnet exists
- Both nodes have the VPC bridge (`syfbr-{vpc_id}`) and VXLAN interface (`syfvx-{vpc_id}`) created

## Steps

### 1. Create a VM on node-1

```bash
syfrah compute vm create --name fdb-test-vm --image alpine-3.20 --subnet test-sub \
  --project test-proj --org test-org --vcpus 1 --memory 512
```

Record the VM IP, MAC, and hosting node fabric IPv6 from the output.

### 2. Verify FDB entry on node-2

On node-2, check that a static FDB entry was added for the VM's MAC pointing to node-1's fabric IPv6:

```bash
bridge fdb show dev syfvx-{vpc_id} | grep {vm_mac}
```

**Expected**: entry exists with `dst {node1_fabric_ipv6}`.

### 3. Verify ARP proxy on node-2

On node-2, check the neighbor table on the VXLAN interface:

```bash
ip neigh show dev syfvx-{vpc_id} | grep {vm_ip}
```

**Expected**: entry exists with `lladdr {vm_mac} PERMANENT`.

### 4. Verify local node skips self

On node-1, verify there is no FDB entry for its own VM on the VXLAN device (local traffic uses the bridge directly):

```bash
bridge fdb show dev syfvx-{vpc_id} | grep {vm_mac}
```

**Expected**: no matching entry (local VMs are resolved via the bridge, not VXLAN).

### 5. Delete the VM

```bash
syfrah compute vm delete --name fdb-test-vm --project test-proj --org test-org
```

### 6. Verify FDB removal on node-2

```bash
bridge fdb show dev syfvx-{vpc_id} | grep {vm_mac}
```

**Expected**: no matching entry.

### 7. Verify ARP proxy removal on node-2

```bash
ip neigh show dev syfvx-{vpc_id} | grep {vm_ip}
```

**Expected**: no matching entry.

### 8. Idempotent re-deletion

Delete the same VM again (or send a duplicate Remove announcement):

```bash
syfrah compute vm delete --name fdb-test-vm --project test-proj --org test-org
```

**Expected**: command succeeds without error (idempotent removal).

## Cleanup

```bash
syfrah subnet delete test-sub
syfrah env destroy test-env --project test-proj --org test-org
syfrah project delete test-proj --org test-org
syfrah org delete test-org
```
