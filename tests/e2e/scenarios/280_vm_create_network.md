# 280 — VM create with network integration

## Goal
Verify that `vm create --subnet` resolves the subnet, allocates an IP via IPAM,
creates the network plumbing (bridge, VXLAN, TAP, nftables, NAT), stores the
VM placement, and boots the VM with correct network config-drive settings.

## Prerequisites
- Org hierarchy created: org `acme`, project `backend`, env `production`
- VPC `default` with CIDR `10.0.0.0/16` and VNI 100
- Subnet `frontend` with CIDR `10.0.1.0/24` in the VPC
- Image `alpine-3.20` available locally

## Steps

### 1. Create VM with subnet
```bash
syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048 --ssh-key ~/.ssh/id.pub
```
**Expected**: VM created with IP `10.0.1.3`, MAC `02:00:0a:00:01:03`.

### 2. Verify network interfaces
```bash
ip link show syfbr-vpc-default     # bridge exists
ip link show syfvx-vpc-default     # VXLAN exists
ip link show syftap-web-1          # TAP exists, attached to bridge
```
**Expected**: all three interfaces exist and are UP.

### 3. Verify bridge gateway
```bash
ip addr show syfbr-vpc-default | grep 10.0.1.1
```
**Expected**: gateway IP `10.0.1.1/24` present on bridge.

### 4. Verify nftables rules
```bash
nft list ruleset | grep syftap-web-1
```
**Expected**: anti-spoofing rule for MAC `02:00:0a:00:01:03` and IP `10.0.1.3`.

### 5. Verify NAT
```bash
nft list ruleset | grep masquerade
```
**Expected**: SNAT masquerade rule for subnet `10.0.1.0/24`.

### 6. Verify IPAM allocation
```bash
syfrah org ipam list --subnet frontend
```
**Expected**: IP `10.0.1.3` in `Assigned` state, linked to VM `web-1`.

### 7. Verify VM placement
Internally stored in redb `vm_placements` table. Confirm via:
```bash
syfrah compute vm get web-1 --project backend --org acme
```
**Expected**: output includes IP, subnet, VPC, node info.

### 8. Verify cloud-init network config
Inside the VM:
```bash
ssh root@10.0.1.3 "ip addr show eth0"
ssh root@10.0.1.3 "ip route show default"
ssh root@10.0.1.3 "cat /etc/resolv.conf"
```
**Expected**:
- eth0 has `10.0.1.3/24`
- Default route via `10.0.1.1`
- DNS `8.8.8.8` and `1.1.1.1`
- MTU 1350

### 9. Create second VM, verify sequential IP
```bash
syfrah compute vm create --name web-2 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id.pub
```
**Expected**: IP `10.0.1.4`.

### 10. Same-subnet connectivity
```bash
ssh root@10.0.1.3 "ping -c 3 10.0.1.4"
```
**Expected**: ping succeeds (same bridge, local FDB).

### 11. Delete VM, verify cleanup
```bash
syfrah compute vm delete web-1 --project backend --org acme
```
**Expected**:
- TAP `syftap-web-1` deleted
- nftables rules for `syftap-web-1` removed
- IP `10.0.1.3` released from IPAM
- Placement removed

### 12. Rollback on failure
Simulate a failure after IP allocation (e.g., TAP creation fails):
**Expected**: IP allocation is rolled back, no orphaned resources.

## Pass criteria
- All 12 steps produce the expected output
- No orphaned network interfaces after VM delete
- IPAM bitmap matches actual allocations
- No nftables rules leak after VM delete
