# Test: Multi-subnet per environment

## Objective

- Verify that an environment can have multiple subnets (N subnets in same env)
- Verify auto-allocated CIDRs do not overlap across subnets in the same VPC
- Verify `list_subnets_by_env()` returns all subnets for an environment
- Document that `vm create` must require `--subnet` when multiple subnets exist

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing orgs, projects, environments, VPCs, or subnets)

## Steps

### 1. Set up the org hierarchy

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
```

Expected: all three created successfully.

### 2. Create the first subnet (auto-allocated CIDR)

```bash
syfrah subnet create frontend --env production --project backend --org acme
```

Expected output:
```
Subnet created: frontend
  CIDR:    10.0.0.0/24
  Gateway: 10.0.0.1
  VPC:     acme-backend-default
  Env:     production
```

The default VPC is auto-created on first subnet. CIDR is auto-allocated as the first /24 in the VPC's range.

### 3. Create a second subnet in the same environment

```bash
syfrah subnet create database --env production --project backend --org acme
```

Expected output:
```
Subnet created: database
  CIDR:    10.0.1.0/24
  Gateway: 10.0.1.1
  VPC:     acme-backend-default
  Env:     production
```

CIDR is auto-allocated as the next available /24. No overlap with the first subnet.

### 4. Create a third subnet in the same environment

```bash
syfrah subnet create internal --env production --project backend --org acme
```

Expected output:
```
Subnet created: internal
  CIDR:    10.0.2.0/24
  Gateway: 10.0.2.1
  VPC:     acme-backend-default
  Env:     production
```

### 5. List subnets for the environment

```bash
syfrah subnet list --env production --project backend --org acme
```

Expected output (3 subnets listed):
```
NAME        CIDR           GATEWAY     VPC                    ENV
frontend    10.0.0.0/24    10.0.0.1    acme-backend-default   production
database    10.0.1.0/24    10.0.1.1    acme-backend-default   production
internal    10.0.2.0/24    10.0.2.1    acme-backend-default   production
```

### 6. Verify CIDRs do not overlap

All three CIDRs are distinct /24 blocks within the VPC's /16 range:
- `10.0.0.0/24` (frontend)
- `10.0.1.0/24` (database)
- `10.0.2.0/24` (internal)

No overlap exists between any pair.

### 7. VM create without --subnet (expected failure)

When multiple subnets exist in the environment:

```bash
syfrah compute vm create --name web-1 --image alpine-3.20 --project backend --org acme
```

Expected error:
```
error: environment 'production' has 3 subnets -- specify --subnet <name>
```

### 8. VM create with --subnet (expected success path)

```bash
syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend --project backend --org acme
```

Expected: VM is created in the `frontend` subnet and gets an IP from `10.0.0.0/24`.

### 9. Single-subnet auto-selection

Create a separate environment with only one subnet:

```bash
syfrah env create staging --project backend --org acme
syfrah subnet create default --env staging --project backend --org acme
syfrah compute vm create --name test-1 --image alpine-3.20 --project backend --org acme --env staging
```

Expected: VM is created successfully without `--subnet` because only one subnet exists.

## Expected Results

- Three subnets coexist in the same environment and VPC
- Auto-allocated CIDRs are sequential /24 blocks with no overlaps
- Gateway IPs are always `.1` of each subnet CIDR
- `list_subnets_by_env()` returns all subnets for the given environment
- `vm create` requires `--subnet` when multiple subnets exist
- `vm create` auto-selects the subnet when only one exists

## Failure Criteria

- Subnet creation fails when a valid CIDR is available
- Two subnets in the same VPC receive overlapping CIDRs
- `list_subnets_by_env()` misses subnets or returns subnets from other environments
- `vm create` succeeds without `--subnet` when multiple subnets exist
- Gateway IP is not `.1` of the subnet CIDR
