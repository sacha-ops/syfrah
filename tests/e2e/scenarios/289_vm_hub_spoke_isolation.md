# 289 — Hub and spoke isolation

## Goal
Verify hub-and-spoke VPC topology: spokes can reach the hub but CANNOT
reach each other. Peering is NOT transitive.

## Prerequisites
- Syfrah daemon running
- Image `alpine-3.20` available locally
- Org hierarchy: org `acme`, project `infra`, env `production`

## Steps

### 1. Create three VPCs (hub + 2 spokes)
```bash
syfrah org vpc create hub --cidr 10.100.0.0/16 --env production --project infra --org acme
syfrah org vpc create spoke-a --cidr 10.101.0.0/16 --env production --project infra --org acme
syfrah org vpc create spoke-b --cidr 10.102.0.0/16 --env production --project infra --org acme
```

### 2. Create subnets in each VPC
```bash
syfrah org subnet create hub-net --cidr 10.100.1.0/24 --vpc hub --env production --project infra --org acme
syfrah org subnet create spoke-a-net --cidr 10.101.1.0/24 --vpc spoke-a --env production --project infra --org acme
syfrah org subnet create spoke-b-net --cidr 10.102.1.0/24 --vpc spoke-b --env production --project infra --org acme
```

### 3. Create VMs
```bash
syfrah compute vm create --name hub-vm --image alpine-3.20 \
  --subnet hub-net --env production --project infra --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub

syfrah compute vm create --name spoke-a-vm --image alpine-3.20 \
  --subnet spoke-a-net --env production --project infra --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub

syfrah compute vm create --name spoke-b-vm --image alpine-3.20 \
  --subnet spoke-b-net --env production --project infra --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```

### 4. Peer hub with each spoke
```bash
syfrah org vpc peer hub spoke-a --env production --project infra --org acme
syfrah org vpc peer hub spoke-b --env production --project infra --org acme
```
**Expected**: two peerings created. No peering between spoke-a and spoke-b.

### 5. Verify spoke-a can reach hub
```bash
ssh root@10.101.1.3 "ping -c 3 -W 5 10.100.1.3"
```
**Expected**: ping succeeds, 0% packet loss.

### 6. Verify spoke-b can reach hub
```bash
ssh root@10.102.1.3 "ping -c 3 -W 5 10.100.1.3"
```
**Expected**: ping succeeds, 0% packet loss.

### 7. Verify hub can reach both spokes
```bash
ssh root@10.100.1.3 "ping -c 3 -W 5 10.101.1.3"
ssh root@10.100.1.3 "ping -c 3 -W 5 10.102.1.3"
```
**Expected**: both pings succeed.

### 8. Verify spoke-a CANNOT reach spoke-b (critical)
```bash
ssh root@10.101.1.3 "ping -c 3 -W 3 10.102.1.3"
```
**Expected**: 100% packet loss. Peering is NOT transitive.

### 9. Verify spoke-b CANNOT reach spoke-a (critical)
```bash
ssh root@10.102.1.3 "ping -c 3 -W 3 10.101.1.3"
```
**Expected**: 100% packet loss.

### 10. Cleanup
```bash
syfrah compute vm delete hub-vm
syfrah compute vm delete spoke-a-vm
syfrah compute vm delete spoke-b-vm
```

## Pass criteria
- Spoke-to-hub connectivity works in both directions
- Spoke-to-spoke connectivity FAILS (no transitive peering)
- This is a SECURITY test: spoke isolation is critical
- Steps 8 and 9 are the most important — they MUST fail
