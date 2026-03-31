# 290 — Shared VPC cross-project

## Goal
Verify that a shared VPC at the org level can be attached to multiple
projects, and VMs in different projects can communicate within the
shared VPC.

## Prerequisites
- Syfrah daemon running
- Image `alpine-3.20` available locally
- Org `acme` created

## Steps

### 1. Create two projects
```bash
syfrah org project create team-alpha --org acme
syfrah org project create team-beta --org acme
```

### 2. Create environments in each project
```bash
syfrah org env create staging --project team-alpha --org acme
syfrah org env create staging --project team-beta --org acme
```

### 3. Create a shared VPC at the org level
```bash
syfrah org vpc create shared-infra --cidr 10.50.0.0/16 --shared --org acme
```
**Expected**: shared VPC created, accessible by all projects in the org.

### 4. Attach the shared VPC to both projects
```bash
syfrah org vpc attach shared-infra --project team-alpha --org acme
syfrah org vpc attach shared-infra --project team-beta --org acme
```

### 5. Create subnets in the shared VPC for each project
```bash
syfrah org subnet create alpha-net --cidr 10.50.1.0/24 --vpc shared-infra --env staging --project team-alpha --org acme
syfrah org subnet create beta-net --cidr 10.50.2.0/24 --vpc shared-infra --env staging --project team-beta --org acme
```

### 6. Create VMs in each project
```bash
syfrah compute vm create --name alpha-vm --image alpine-3.20 \
  --subnet alpha-net --env staging --project team-alpha --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub

syfrah compute vm create --name beta-vm --image alpine-3.20 \
  --subnet beta-net --env staging --project team-beta --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: both VMs on the same VPC bridge despite being in different projects.

### 7. Verify same bridge
```bash
bridge link show | grep -E "syftap-(alpha|beta)-vm"
```
**Expected**: both TAPs attached to the same `syfbr-vpc-shared-infra` bridge.

### 8. Ping from alpha to beta
```bash
ssh root@10.50.1.3 "ping -c 3 -W 5 10.50.2.3"
```
**Expected**: ping succeeds — shared VPC allows cross-project communication.

### 9. Ping from beta to alpha
```bash
ssh root@10.50.2.3 "ping -c 3 -W 5 10.50.1.3"
```
**Expected**: bidirectional connectivity works.

### 10. Cleanup
```bash
syfrah compute vm delete alpha-vm
syfrah compute vm delete beta-vm
```

## Pass criteria
- VMs in different projects share the same VPC bridge
- Cross-project ping succeeds within the shared VPC
- Shared VPC is accessible from both projects
- Subnet isolation within the shared VPC works correctly
