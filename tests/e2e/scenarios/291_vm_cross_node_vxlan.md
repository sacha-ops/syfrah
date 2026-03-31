# 291 — Multi-node cross-AZ over VXLAN

## Goal
Verify that VMs on different nodes in the same subnet can communicate
over the VXLAN/WireGuard tunnel. This tests the full overlay stack:
VXLAN encapsulation, FDB entries, ARP proxy, and WireGuard transport.

## Prerequisites
- Two nodes (`node-1`, `node-2`) in a WireGuard fabric mesh
- Both nodes running the Syfrah daemon
- Image `alpine-3.20` available on both nodes
- Org hierarchy with VPC and subnet configured on both nodes
- Fabric mesh verified: `syfrah fabric peers` shows both nodes

## Steps

### 1. Verify fabric mesh
```bash
# On node-1:
syfrah fabric peers
```
**Expected**: `node-2` listed as a peer with status `Connected`.

### 2. Create VM on node-1
```bash
# On node-1:
syfrah compute vm create --name vm-node1 --image alpine-3.20 \
  --subnet frontend --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP `10.0.1.3` on node-1.

### 3. Create VM on node-2
```bash
# On node-2:
syfrah compute vm create --name vm-node2 --image alpine-3.20 \
  --subnet frontend --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP `10.0.1.4` on node-2.

### 4. Verify VXLAN tunnels on both nodes
```bash
# On node-1:
ip link show syfvx-vpc-default
bridge fdb show dev syfvx-vpc-default | grep -v 00:00:00:00:00:00
```
**Expected**: VXLAN interface exists, FDB entry for node-2's VTEP IP.

```bash
# On node-2:
ip link show syfvx-vpc-default
bridge fdb show dev syfvx-vpc-default | grep -v 00:00:00:00:00:00
```
**Expected**: VXLAN interface exists, FDB entry for node-1's VTEP IP.

### 5. Verify ARP proxy entries
```bash
# On node-1:
ip neigh show dev syfvx-vpc-default
```
**Expected**: ARP proxy entry for `10.0.1.4` (node-2's VM) with its MAC.

### 6. Ping from node-1 VM to node-2 VM
```bash
ssh root@10.0.1.3 "ping -c 5 -W 10 10.0.1.4"
```
**Expected**: 5 packets transmitted, 5 received, 0% packet loss.
Latency should include WireGuard + VXLAN overhead.

### 7. Ping from node-2 VM to node-1 VM
```bash
ssh root@10.0.1.4 "ping -c 5 -W 10 10.0.1.3"
```
**Expected**: bidirectional connectivity over VXLAN tunnel.

### 8. Verify traffic path (optional)
```bash
# On node-1, capture VXLAN traffic:
tcpdump -i wg0 -c 10 udp port 4789
```
While running ping from step 6. **Expected**: VXLAN-encapsulated packets
visible on the WireGuard interface.

### 9. Test MTU
```bash
ssh root@10.0.1.3 "ping -c 1 -s 1300 -M do 10.0.1.4"
```
**Expected**: succeeds (MTU 1350 accommodates 1300 + headers).

```bash
ssh root@10.0.1.3 "ping -c 1 -s 1400 -M do 10.0.1.4"
```
**Expected**: fails (exceeds VXLAN MTU after encapsulation overhead).

### 10. Cleanup
```bash
# On node-1:
syfrah compute vm delete vm-node1
# On node-2:
syfrah compute vm delete vm-node2
```
**Expected**: VMs deleted, FDB entries and ARP proxy entries removed on both nodes.

## Pass criteria
- Cross-node ping succeeds with 0% packet loss
- VXLAN encapsulation verified via FDB entries
- ARP proxy correctly resolves remote VM MACs
- MTU is respected (1350 overlay MTU)
- FDB entries cleaned up on VM deletion
- This test requires a real 2-node setup — cannot be run on a single node
