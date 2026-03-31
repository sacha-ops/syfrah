# 285 — Cross-subnet ping (same VPC, same node)

## Goal
Verify that two VMs in different subnets of the same VPC can communicate
via bridge routing on a single node.

## Prerequisites
- Syfrah daemon running
- Image `alpine-3.20` available locally
- Org hierarchy: org `acme`, project `backend`, env `production`
- VPC `default` with CIDR `10.0.0.0/16` and VNI 100

## Steps

### 1. Create two subnets in the same VPC
```bash
syfrah org subnet create frontend --cidr 10.0.1.0/24 --vpc default --env production --project backend --org acme
syfrah org subnet create backend-net --cidr 10.0.2.0/24 --vpc default --env production --project backend --org acme
```
**Expected**: both subnets created successfully.

### 2. Create VM in first subnet
```bash
syfrah compute vm create --name web-1 --image alpine-3.20 \
  --subnet frontend --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP `10.0.1.3`.

### 3. Create VM in second subnet
```bash
syfrah compute vm create --name api-1 --image alpine-3.20 \
  --subnet backend-net --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP `10.0.2.3`.

### 4. Verify both VMs share the same bridge
```bash
ip link show syfbr-vpc-default
bridge link show | grep syftap
```
**Expected**: both `syftap-web-1` and `syftap-api-1` attached to `syfbr-vpc-default`.

### 5. Ping from web-1 to api-1
```bash
ssh root@10.0.1.3 "ping -c 3 -W 5 10.0.2.3"
```
**Expected**: 3 packets transmitted, 3 received, 0% packet loss.

### 6. Ping from api-1 to web-1
```bash
ssh root@10.0.2.3 "ping -c 3 -W 5 10.0.1.3"
```
**Expected**: 3 packets transmitted, 3 received, 0% packet loss.

### 7. Verify routing inside VMs
```bash
ssh root@10.0.1.3 "ip route show"
ssh root@10.0.2.3 "ip route show"
```
**Expected**: default route via respective subnet gateways (`10.0.1.1`, `10.0.2.1`).

### 8. Cleanup
```bash
syfrah compute vm delete web-1
syfrah compute vm delete api-1
```
**Expected**: both VMs deleted, TAPs removed, IPs released.

## Pass criteria
- Bidirectional ping succeeds between VMs in different subnets
- Both VMs share the same VPC bridge
- Routing tables correctly configured inside each VM
- No packet loss on ping
