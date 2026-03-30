# Test: Subnet CLI -- Create, List, Delete Subnets

## Objective

Verify that the `syfrah subnet` CLI commands correctly create, list, and delete
subnets with proper VPC resolution, auto-creation, CIDR allocation, and error
handling.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Clean state (no existing organizations, projects, environments, or VPCs)

## Steps

### 1. Set up org, project, and environment

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
```

**Expected:** All commands succeed with confirmation messages.

### 2. Create a subnet (auto-create default VPC, auto-allocate CIDR)

```bash
syfrah subnet create frontend --env production --project backend --org acme
```

**Expected output:**
```
Subnet created: frontend
  VPC:      acme-backend-default
  Env:      production
  CIDR:     10.0.0.0/24
  Gateway:  10.0.0.1
  Created:  <date>
```

A default VPC should be auto-created for the project with an auto-allocated /16 CIDR.

### 3. Create a second subnet (auto-allocate next /24)

```bash
syfrah subnet create database --env production --project backend --org acme
```

**Expected output includes:**
```
  CIDR:     10.0.1.0/24
  Gateway:  10.0.1.1
```

The second subnet should get the next available /24 within the same default VPC.

### 4. Create a VPC and a subnet with explicit CIDR

```bash
syfrah vpc create custom-vpc --project backend --org acme --cidr 10.2.0.0/16
syfrah subnet create api --env production --project backend --org acme --vpc custom-vpc --cidr 10.2.1.0/24
```

**Expected output includes:**
```
Subnet created: api
  VPC:      custom-vpc
  CIDR:     10.2.1.0/24
  Gateway:  10.2.1.1
```

### 5. Attempt to create a duplicate subnet

```bash
syfrah subnet create frontend --env production --project backend --org acme
```

**Expected:** Exit code 1, error containing `subnet already exists`

### 6. Attempt to create a subnet with CIDR outside VPC range

```bash
syfrah subnet create bad-cidr --env production --project backend --org acme --vpc custom-vpc --cidr 10.99.0.0/24
```

**Expected:** Exit code 1, error containing `outside VPC CIDR`

### 7. List subnets (table format)

```bash
syfrah subnet list --project backend --org acme
```

**Expected output:**
- Header row with NAME, VPC, ENV, CIDR, GATEWAY, CREATED columns
- Rows for `frontend`, `database`, and `api`
- Correct VPC names and CIDR blocks

### 8. List subnets filtered by VPC

```bash
syfrah subnet list --vpc custom-vpc
```

**Expected:** Only `api` subnet appears

### 9. List subnets filtered by environment

```bash
syfrah subnet list --env production
```

**Expected:** All three subnets appear (all are in production env)

### 10. List subnets (JSON format)

```bash
syfrah subnet list --project backend --org acme --json
```

**Expected:**
- Valid JSON array
- Each object has `id`, `name`, `vpc_id`, `env_id`, `cidr`, `gateway`, `created_at` fields

### 11. Delete a subnet (with --yes)

```bash
syfrah subnet delete api --vpc custom-vpc --yes
```

**Expected output:**
```
Subnet 'api' deleted from VPC 'custom-vpc'.
```

### 12. Verify deletion

```bash
syfrah subnet list --vpc custom-vpc --json
```

**Expected:** Empty JSON array `[]`

### 13. Attempt to delete a non-existent subnet

```bash
syfrah subnet delete ghost --vpc custom-vpc --yes
```

**Expected:** Exit code 1, error containing `subnet not found`

### 14. Clean up

```bash
syfrah subnet delete frontend --vpc acme-backend-default --yes
syfrah subnet delete database --vpc acme-backend-default --yes
syfrah vpc delete custom-vpc --org acme --yes
syfrah vpc delete acme-backend-default --org acme --yes
syfrah env destroy production --project backend --org acme --yes
syfrah project delete backend --org acme --yes
syfrah org delete acme --yes
```

**Expected:** All commands succeed with confirmation messages.

### 15. Verify empty state

```bash
syfrah subnet list
```

**Expected output:**
```
No subnets found.
```

## Pass criteria

- All commands exit 0 on success, 1 on error
- Default VPC is auto-created when first subnet is created without --vpc
- CIDR auto-allocation assigns sequential /24 blocks within the VPC
- Explicit --vpc and --cidr flags work correctly
- Duplicate subnet names within a VPC are rejected
- Subnet CIDR outside VPC range is rejected
- List shows correct table/JSON output with NAME, VPC, ENV, CIDR, GATEWAY, CREATED columns
- Filtering by VPC, env, and project works correctly
- Deletion by name + VPC works
- State persists across commands (redb)
