# 286 — VPC isolation

## Goal
Verify that VMs in different VPCs CANNOT communicate with each other.
Different VNIs means traffic stays isolated on separate bridges.

## Prerequisites
- Syfrah daemon running
- Image `alpine-3.20` available locally
- Org hierarchy: org `acme`, project `backend`, env `production`

## Steps

### 1. Create two separate VPCs
```bash
syfrah org vpc create vpc-alpha --cidr 10.1.0.0/16 --env production --project backend --org acme
syfrah org vpc create vpc-beta --cidr 10.2.0.0/16 --env production --project backend --org acme
```
**Expected**: two VPCs with different VNIs created.

### 2. Create subnets in each VPC
```bash
syfrah org subnet create net-a --cidr 10.1.1.0/24 --vpc vpc-alpha --env production --project backend --org acme
syfrah org subnet create net-b --cidr 10.2.1.0/24 --vpc vpc-beta --env production --project backend --org acme
```
**Expected**: subnets created in their respective VPCs.

### 3. Create VM in VPC alpha
```bash
syfrah compute vm create --name alpha-vm --image alpine-3.20 \
  --subnet net-a --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP `10.1.1.3`.

### 4. Create VM in VPC beta
```bash
syfrah compute vm create --name beta-vm --image alpine-3.20 \
  --subnet net-b --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP `10.2.1.3`.

### 5. Verify separate bridges
```bash
ip link show syfbr-vpc-alpha
ip link show syfbr-vpc-beta
```
**Expected**: two distinct bridges, each with its own VXLAN and TAP.

### 6. Attempt ping from alpha to beta (must FAIL)
```bash
ssh root@10.1.1.3 "ping -c 3 -W 3 10.2.1.3"
```
**Expected**: 100% packet loss. Ping MUST fail — no connectivity between VPCs.

### 7. Attempt ping from beta to alpha (must FAIL)
```bash
ssh root@10.2.1.3 "ping -c 3 -W 3 10.1.1.3"
```
**Expected**: 100% packet loss. No route between VPCs.

### 8. Verify nftables isolation
```bash
nft list ruleset | grep -E "syfbr-vpc-(alpha|beta)"
```
**Expected**: no forwarding rules between the two VPC bridges.

### 9. Cleanup
```bash
syfrah compute vm delete alpha-vm
syfrah compute vm delete beta-vm
```

## Pass criteria
- Ping between VPCs fails with 100% packet loss
- Each VPC has its own isolated bridge and VXLAN
- No cross-VPC forwarding rules in nftables
- This test PASSES when connectivity FAILS (isolation is the feature)
