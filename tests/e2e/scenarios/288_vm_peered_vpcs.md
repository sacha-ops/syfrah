# 288 — Peered VPCs connectivity

## Goal
Verify that two VPCs can communicate after being peered. VPC peering
adds forwarding rules between the two bridges, allowing cross-VPC traffic.

## Prerequisites
- Syfrah daemon running
- Image `alpine-3.20` available locally
- Org hierarchy: org `acme`, project `backend`, env `production`

## Steps

### 1. Create two VPCs
```bash
syfrah org vpc create vpc-web --cidr 10.10.0.0/16 --env production --project backend --org acme
syfrah org vpc create vpc-db --cidr 10.20.0.0/16 --env production --project backend --org acme
```
**Expected**: two VPCs created with different VNIs.

### 2. Create subnets
```bash
syfrah org subnet create web-net --cidr 10.10.1.0/24 --vpc vpc-web --env production --project backend --org acme
syfrah org subnet create db-net --cidr 10.20.1.0/24 --vpc vpc-db --env production --project backend --org acme
```

### 3. Create VMs in each VPC
```bash
syfrah compute vm create --name web-srv --image alpine-3.20 \
  --subnet web-net --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub

syfrah compute vm create --name db-srv --image alpine-3.20 \
  --subnet db-net --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VMs created in their respective subnets.

### 4. Verify isolation BEFORE peering
```bash
ssh root@10.10.1.3 "ping -c 2 -W 3 10.20.1.3"
```
**Expected**: ping FAILS (100% packet loss) — VPCs are isolated.

### 5. Peer the two VPCs
```bash
syfrah org vpc peer vpc-web vpc-db --env production --project backend --org acme
```
**Expected**: peering established, forwarding rules applied.

### 6. Verify forwarding rules
```bash
nft list ruleset | grep -E "syfbr-vpc-(web|db)"
```
**Expected**: bidirectional forwarding rules between `syfbr-vpc-web` and `syfbr-vpc-db`.

### 7. Ping from web to db AFTER peering
```bash
ssh root@10.10.1.3 "ping -c 3 -W 5 10.20.1.3"
```
**Expected**: 3 packets transmitted, 3 received, 0% packet loss.

### 8. Ping from db to web AFTER peering
```bash
ssh root@10.20.1.3 "ping -c 3 -W 5 10.10.1.3"
```
**Expected**: bidirectional connectivity works.

### 9. Cleanup
```bash
syfrah compute vm delete web-srv
syfrah compute vm delete db-srv
```

## Pass criteria
- No connectivity between VPCs before peering
- Full bidirectional connectivity after peering
- Forwarding rules present in nftables after peering
- Peering is symmetric (both directions work)
