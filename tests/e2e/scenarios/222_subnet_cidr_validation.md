# Test: Subnet CIDR validation — in VPC range, no overlap

## Objective

- Subnet CIDRs must be contained within the parent VPC's CIDR
- Subnet CIDRs within the same VPC must not overlap
- Subnet prefix length must be between /24 and /28
- Valid, non-overlapping subnets within a VPC are accepted

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Org, project, and environment created:
  ```bash
  syfrah org create test-org
  syfrah project create backend --org test-org
  syfrah env create production --project backend --org test-org
  ```
- A VPC with CIDR 10.1.0.0/16:
  ```bash
  syfrah vpc create test-vpc --org test-org --cidr 10.1.0.0/16
  ```

## Steps

### 1. Create a valid subnet within the VPC

```bash
syfrah subnet create web --env production --project backend --org test-org --vpc test-vpc --cidr 10.1.1.0/24
```

Expected: subnet created successfully with CIDR 10.1.1.0/24.

### 2. Reject subnet CIDR outside VPC range

```bash
syfrah subnet create outside --env production --project backend --org test-org --vpc test-vpc --cidr 10.2.0.0/24
```

Expected: error indicating subnet CIDR 10.2.0.0/24 is not within VPC CIDR 10.1.0.0/16.

### 3. Reject overlapping subnet

```bash
syfrah subnet create overlap --env production --project backend --org test-org --vpc test-vpc --cidr 10.1.1.0/24
```

Expected: error indicating CIDR 10.1.1.0/24 overlaps with the existing subnet.

### 4. Reject partial overlap (smaller subnet inside existing)

```bash
syfrah subnet create partial --env production --project backend --org test-org --vpc test-vpc --cidr 10.1.1.0/28
```

Expected: error indicating CIDR overlap.

### 5. Create a second non-overlapping subnet

```bash
syfrah subnet create db --env production --project backend --org test-org --vpc test-vpc --cidr 10.1.2.0/24
```

Expected: subnet created successfully with CIDR 10.1.2.0/24.

### 6. Create a /28 subnet (smallest allowed)

```bash
syfrah subnet create cache --env production --project backend --org test-org --vpc test-vpc --cidr 10.1.3.0/28
```

Expected: subnet created successfully with CIDR 10.1.3.0/28.

### 7. Reject subnet prefix too small (/16)

```bash
syfrah subnet create huge --env production --project backend --org test-org --vpc test-vpc --cidr 10.1.0.0/16
```

Expected: error indicating prefix length must be between /24 and /28.

### 8. List subnets and verify

```bash
syfrah subnet list --vpc test-vpc
```

Expected: exactly 3 subnets listed (web 10.1.1.0/24, db 10.1.2.0/24, cache 10.1.3.0/28).

## Expected results

- Steps 1, 5, 6 succeed with subnets created
- Steps 2, 3, 4, 7 fail with descriptive error messages
- Step 8 shows exactly 3 subnets

## Failure criteria

- A subnet CIDR outside the VPC range is accepted
- Overlapping subnet CIDRs within the same VPC are accepted
- A subnet with a prefix smaller than /24 or larger than /28 is accepted
- A valid, non-overlapping subnet is rejected
