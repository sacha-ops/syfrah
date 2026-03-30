# Test: Overlay FDB management — static entries + ARP proxy

## Objective

Verify that static FDB entries and ARP proxy entries are correctly added
and removed on a real Linux host, ensuring deterministic forwarding
without flood-and-learn.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Root access (NET_ADMIN capability)
- `iproute2` installed (bridge, ip commands)
- Kernel VXLAN module loaded (`modprobe vxlan`)
- The syfrah daemon is running with at least one fabric peer

## Steps

### 1. Create a VPC with a subnet and two VMs

```bash
syfrah org create fdb-test-org
syfrah project create fdb-proj --org fdb-test-org
syfrah env create fdb-env --project fdb-proj --org fdb-test-org
syfrah subnet create fdb-subnet --env fdb-env --project fdb-proj --org fdb-test-org
```

### 2. Verify VXLAN and bridge interfaces exist

After the subnet is created and a VM is placed:

```bash
ip link show type vxlan | grep syfvx-
ip link show type bridge | grep syfbr-
```

Expected: at least one `syfvx-*` and one `syfbr-*` interface.

### 3. Create a VM and verify FDB entry

```bash
syfrah compute vm create --name fdb-vm-1 --image alpine-3.20 \
  --subnet fdb-subnet --project fdb-proj --org fdb-test-org \
  --vcpus 1 --memory 512 --ssh-key ~/.ssh/id.pub
```

Check the FDB table for the VM's MAC:

```bash
bridge fdb show dev syfvx-* | grep "02:00:"
```

Expected: a static entry with the VM's MAC address.

### 4. Verify ARP proxy neighbor entry

```bash
ip neigh show dev syfvx-* | grep "PERMANENT"
```

Expected: the VM's IP mapped to its MAC with `PERMANENT` state.

### 5. Delete the VM and verify cleanup

```bash
syfrah compute vm delete fdb-vm-1 --project fdb-proj --org fdb-test-org
```

Verify FDB entry is removed:

```bash
bridge fdb show dev syfvx-* | grep "02:00:"
```

Expected: no entry for the deleted VM's MAC.

Verify ARP proxy is removed:

```bash
ip neigh show dev syfvx-* | grep "PERMANENT"
```

Expected: no permanent neighbor entry for the deleted VM's IP.

### 6. Multi-node FDB test (requires 2 servers)

On node-1:
```bash
syfrah compute vm create --name fdb-remote-1 --image alpine-3.20 \
  --subnet fdb-subnet --project fdb-proj --org fdb-test-org \
  --vcpus 1 --memory 512 --ssh-key ~/.ssh/id.pub
```

On node-2, verify the FDB entry points to node-1's fabric IPv6:
```bash
bridge fdb show dev syfvx-* | grep "02:00:"
```

Expected: the entry's `dst` field is node-1's fabric IPv6 address.

Verify the ARP proxy on node-2:
```bash
ip neigh show dev syfvx-* nud permanent
```

Expected: fdb-remote-1's IP mapped to its MAC.

### 7. Cleanup

```bash
syfrah compute vm delete fdb-remote-1 --project fdb-proj --org fdb-test-org
syfrah subnet delete fdb-subnet --project fdb-proj --org fdb-test-org
syfrah env destroy fdb-env --project fdb-proj --org fdb-test-org
syfrah project delete fdb-proj --org fdb-test-org
syfrah org delete fdb-test-org
```

## Expected results

- FDB entries are static (added by the control plane, not learned).
- Each VM's MAC resolves to the correct VTEP (remote node's fabric IPv6).
- ARP proxy entries are permanent and answer ARP requests locally.
- Deleting a VM removes both FDB and ARP proxy entries.
- No broadcast or flood-and-learn traffic observed.

## Failure criteria

- FDB entry missing after VM creation.
- ARP proxy entry missing after VM creation.
- Stale FDB or ARP entry remains after VM deletion.
- FDB entry points to wrong VTEP address.
- ARP resolution requires broadcast (proxy not working).
