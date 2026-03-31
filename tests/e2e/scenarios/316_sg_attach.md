# Test: Security Group Attach/Detach to VM via NIC

## Objective

- Security groups can be attached to and detached from VMs via their primary NIC
- VPC mismatch is rejected (SG and VM must be in the same VPC)
- Detaching the last SG from a NIC is rejected (a NIC must always have at least one SG)
- Listing attached SGs returns the correct set

## Prerequisites

- A running syfrah daemon with fabric initialized
- An organization, project, environment, VPC, and subnet created
- At least one VM running in the subnet

## Steps

### 1. Set up the environment

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
syfrah vpc create test-vpc --project backend --org acme --cidr 10.1.0.0/16
syfrah subnet create web-tier --env production --project backend --org acme --vpc test-vpc
```

### 2. Create security groups

```bash
syfrah sg create default --vpc test-vpc --description "Default SG for test-vpc"
syfrah sg create web-sg --vpc test-vpc --description "Web tier security group"
syfrah sg create api-sg --vpc test-vpc --description "API tier security group"
```

Verify:
```bash
syfrah sg list --vpc test-vpc
```

Expected: 3 security groups listed (default, web-sg, api-sg).

### 3. Create a VM

```bash
syfrah compute vm create web-1 --image alpine-3.20 --vcpus 1 --memory 512 \
  --subnet web-tier --env production --project backend --org acme
```

### 4. Attach a security group to the VM

```bash
syfrah sg attach web-sg --vm web-1
```

Expected output:
```
Security group 'web-sg' attached to VM 'web-1'.
  NIC 'web-1-nic' now has 2 security group(s). nftables refresh marked.
```

### 5. List attached security groups

```bash
syfrah sg list-attached --vm web-1
```

Expected: both `default` and `web-sg` listed.

### 6. Attach another security group

```bash
syfrah sg attach api-sg --vm web-1
```

Expected: NIC now has 3 security groups.

### 7. Detach a security group

```bash
syfrah sg detach web-sg --vm web-1
```

Expected output:
```
Security group 'web-sg' detached from VM 'web-1'.
  NIC 'web-1-nic' now has 2 security group(s). nftables refresh marked.
```

### 8. Verify VPC mismatch rejection

Create a second VPC and SG:
```bash
syfrah vpc create other-vpc --project backend --org acme --cidr 10.2.0.0/16
syfrah sg create other-sg --vpc other-vpc --description "Different VPC SG"
```

Attempt to attach:
```bash
syfrah sg attach other-sg --vm web-1
```

Expected: error message indicating VPC mismatch.

### 9. Verify cannot detach last SG

Detach api-sg so only default remains:
```bash
syfrah sg detach api-sg --vm web-1
```

Then attempt to detach the last SG:
```bash
syfrah sg detach default --vm web-1
```

Expected: error message indicating cannot detach the last security group.

## Pass Criteria

1. `syfrah sg attach <sg> --vm <vm>` succeeds and reports the updated SG count
2. `syfrah sg detach <sg> --vm <vm>` succeeds and reports the updated SG count
3. `syfrah sg list-attached --vm <vm>` shows the correct set of attached SGs
4. Attaching an SG from a different VPC is rejected with a clear error
5. Detaching the last SG is rejected with a clear error
6. After each attach/detach, nftables refresh is marked for the VM
