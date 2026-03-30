# Test: nftables subnet and VPC isolation

## Objective

- Cross-subnet traffic within the same VPC is blocked by default
- Cross-VPC traffic is blocked by default
- Same-subnet traffic is allowed (normal bridge forwarding)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with overlay networking enabled
- `nftables` installed and active
- At least one VPC with two subnets created
- A second VPC created for cross-VPC testing
- VMs deployed in each subnet/VPC

## Steps

### 1. Set up the environment

Create org, project, and environment:
```bash
syfrah org create test-org
syfrah project create test-proj --org test-org
syfrah env create test-env --project test-proj --org test-org
```

Create two subnets in the default VPC:
```bash
syfrah subnet create subnet-a --env test-env --project test-proj --org test-org --cidr 10.1.1.0/24
syfrah subnet create subnet-b --env test-env --project test-proj --org test-org --cidr 10.1.2.0/24
```

Create a second VPC with its own subnet:
```bash
syfrah vpc create vpc-isolated --project test-proj --org test-org --cidr 10.2.0.0/16
syfrah subnet create subnet-c --env test-env --project test-proj --org test-org --vpc vpc-isolated --cidr 10.2.1.0/24
```

Create VMs:
```bash
syfrah compute vm create --name vm-a1 --image alpine-3.20 --subnet subnet-a --project test-proj --org test-org
syfrah compute vm create --name vm-a2 --image alpine-3.20 --subnet subnet-a --project test-proj --org test-org
syfrah compute vm create --name vm-b1 --image alpine-3.20 --subnet subnet-b --project test-proj --org test-org
syfrah compute vm create --name vm-c1 --image alpine-3.20 --subnet subnet-c --project test-proj --org test-org
```

### 2. Verify same-subnet traffic is allowed

SSH into vm-a1 and ping vm-a2:
```bash
ssh root@<vm-a1-ip> "ping -c 3 -W 2 <vm-a2-ip>"
```

**Expected**: ping succeeds (0% packet loss).

### 3. Verify cross-subnet traffic is blocked

SSH into vm-a1 and ping vm-b1 (different subnet, same VPC):
```bash
ssh root@<vm-a1-ip> "ping -c 3 -W 2 <vm-b1-ip>"
```

**Expected**: ping fails (100% packet loss). nftables forward rule drops the traffic.

### 4. Verify cross-VPC traffic is blocked

SSH into vm-a1 and ping vm-c1 (different VPC):
```bash
ssh root@<vm-a1-ip> "ping -c 3 -W 2 <vm-c1-ip>"
```

**Expected**: ping fails (100% packet loss). nftables forward rule between bridges drops the traffic.

### 5. Verify nftables rules exist

On the host, list the syfrah nftables rules:
```bash
nft list table inet syfrah
```

**Expected**:
- A forward drop rule for traffic from subnet-a CIDR to subnet-b CIDR on the VPC bridge
- A forward drop rule for traffic from subnet-b CIDR to subnet-a CIDR on the VPC bridge
- A forward drop rule between the default VPC bridge and the isolated VPC bridge
- No drop rule for traffic within subnet-a or within subnet-b

## Expected Results

| Scenario | Result |
|---|---|
| vm-a1 -> vm-a2 (same subnet) | PASS (ping succeeds) |
| vm-a1 -> vm-b1 (cross-subnet, same VPC) | BLOCKED (ping fails) |
| vm-a1 -> vm-c1 (cross-VPC) | BLOCKED (ping fails) |
| nftables subnet isolation rules present | Yes |
| nftables VPC isolation rules present | Yes |

## Failure Criteria

- Same-subnet ping fails (bridge forwarding broken)
- Cross-subnet ping succeeds (isolation not enforced)
- Cross-VPC ping succeeds (VPC isolation not enforced)
- nftables rules missing or malformed
- Any VM fails to boot or get an IP

## Cleanup

```bash
syfrah compute vm delete --name vm-c1 --project test-proj --org test-org
syfrah compute vm delete --name vm-b1 --project test-proj --org test-org
syfrah compute vm delete --name vm-a2 --project test-proj --org test-org
syfrah compute vm delete --name vm-a1 --project test-proj --org test-org
syfrah subnet delete subnet-c
syfrah subnet delete subnet-b
syfrah subnet delete subnet-a
syfrah vpc delete vpc-isolated
syfrah env destroy test-env --project test-proj --org test-org
syfrah project delete test-proj --org test-org
syfrah org delete test-org
```
