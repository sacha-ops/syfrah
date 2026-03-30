# Test: Subnet CRUD operations

## Objective

- Create, list, get, and delete subnets via the OrgStore API
- Validate CIDR auto-allocation (next available /24 within VPC range)
- Validate custom CIDR acceptance and range checking
- Verify gateway is always .1 of the subnet CIDR
- Verify filtering by VPC and environment

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing orgs, projects, or VPCs)

## Steps

### 1. Set up hierarchy

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
syfrah vpc create default --project backend --org acme --cidr 10.1.0.0/16
```

Expected: all commands succeed.

### 2. Create a subnet with explicit CIDR

```bash
syfrah subnet create frontend --env production --project backend --org acme --vpc default --cidr 10.1.1.0/24
```

Expected output:
```
Subnet created: frontend
  CIDR:    10.1.1.0/24
  Gateway: 10.1.1.1
  VPC:     default
```

### 3. Create a subnet with auto-allocated CIDR

```bash
syfrah subnet create database --env production --project backend --org acme --vpc default
```

Expected: CIDR is auto-allocated as the next available /24 within 10.1.0.0/16 (e.g. 10.1.0.0/24 if not already taken). Gateway is .1 of the allocated CIDR.

### 4. Create a second auto-allocated subnet

```bash
syfrah subnet create internal --env production --project backend --org acme --vpc default
```

Expected: CIDR is the next sequential /24 that does not overlap with existing subnets. Gateway is .1.

### 5. List subnets by VPC

```bash
syfrah subnet list --vpc default
```

Expected: all three subnets appear (`frontend`, `database`, `internal`).

### 6. List subnets by environment

```bash
syfrah subnet list --env production --project backend --org acme
```

Expected: all three subnets appear (all belong to the same env).

### 7. Reject duplicate subnet name within same VPC

```bash
syfrah subnet create frontend --env production --project backend --org acme --vpc default --cidr 10.1.99.0/24
```

Expected: error indicating subnet already exists. Exit code non-zero.

### 8. Reject CIDR outside VPC range

```bash
syfrah subnet create bad-subnet --env production --project backend --org acme --vpc default --cidr 10.2.0.0/24
```

Expected: error indicating CIDR is outside VPC range. Exit code non-zero.

### 9. Reject overlapping CIDR

```bash
syfrah subnet create overlap --env production --project backend --org acme --vpc default --cidr 10.1.1.0/24
```

Expected: error indicating CIDR overlaps with existing subnet. Exit code non-zero.

### 10. Reject subnet for non-existent VPC

```bash
syfrah subnet create ghost-sub --env production --project backend --org acme --vpc nonexistent
```

Expected: error indicating VPC not found. Exit code non-zero.

### 11. Reject subnet for non-existent environment

```bash
syfrah subnet create ghost-sub --env nonexistent --project backend --org acme --vpc default
```

Expected: error indicating environment not found. Exit code non-zero.

### 12. Delete a subnet

```bash
syfrah subnet delete frontend --vpc default
```

Expected: deletion succeeds. The subnet no longer appears in `syfrah subnet list --vpc default`.

### 13. Delete non-existent subnet

```bash
syfrah subnet delete frontend --vpc default
```

Expected: error indicating subnet not found. Exit code non-zero.

### 14. Verify gateway computation

For each created subnet, verify:
- `10.1.1.0/24` -> gateway `10.1.1.1`
- `10.1.0.0/24` -> gateway `10.1.0.1`
- `10.1.50.0/24` -> gateway `10.1.50.1`

## Pass criteria

- Subnet create/list/get/delete operations succeed as described
- Auto-allocation assigns sequential non-overlapping /24 blocks within the VPC CIDR
- Custom CIDRs are validated against the VPC range
- Overlapping CIDRs are rejected
- Gateway is always .1 of the subnet CIDR
- Non-existent VPCs and environments are rejected with clear error messages
- Duplicate subnet names within the same VPC are rejected
- Deleted subnets do not appear in subsequent list calls
