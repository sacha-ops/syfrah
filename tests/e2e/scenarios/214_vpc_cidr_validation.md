# Test: VPC CIDR validation and overlap detection

## Objective

- Valid CIDRs are accepted during VPC creation
- Invalid CIDRs (bad format, non-private ranges, prefix out of bounds) are rejected
- Overlapping CIDRs within the same org are rejected
- Same CIDR in different orgs is allowed
- Auto-allocation assigns a non-overlapping /16 from 10.0.0.0/8

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- At least one org created (`syfrah org create test-org`)

## Steps

### 1. Create a VPC with a valid explicit CIDR

```bash
syfrah vpc create vpc-alpha --org test-org --cidr 10.1.0.0/16
```

Expected: VPC created successfully with CIDR 10.1.0.0/16 and a unique VNI.

### 2. Reject invalid CIDR — bad format

```bash
syfrah vpc create vpc-bad --org test-org --cidr not-a-cidr
```

Expected: error indicating invalid CIDR format.

### 3. Reject invalid CIDR — non-private range

```bash
syfrah vpc create vpc-public --org test-org --cidr 8.8.8.0/24
```

Expected: error indicating CIDR must be within a private range.

### 4. Reject overlapping CIDR in the same org

```bash
syfrah vpc create vpc-overlap --org test-org --cidr 10.1.0.0/24
```

Expected: error indicating CIDR overlap with vpc-alpha (10.1.0.0/16 contains 10.1.0.0/24).

### 5. Allow same CIDR in a different org

```bash
syfrah org create other-org
syfrah vpc create vpc-other --org other-org --cidr 10.1.0.0/16
```

Expected: VPC created successfully — no cross-org overlap checking.

### 6. Auto-allocate a CIDR when none is given

```bash
syfrah vpc create vpc-auto --org test-org
```

Expected: VPC created with an auto-allocated /16 from 10.0.0.0/8 that does not overlap with 10.1.0.0/16 (e.g. 10.0.0.0/16 or 10.2.0.0/16).

### 7. Reject prefix too large

```bash
syfrah vpc create vpc-tiny --org test-org --cidr 10.5.0.0/29
```

Expected: error indicating prefix length must be between 8 and 28.

### 8. Reject prefix too small

```bash
syfrah vpc create vpc-huge --org test-org --cidr 10.0.0.0/7
```

Expected: error indicating prefix length must be between 8 and 28.

## Expected results

- Steps 1, 5, 6 succeed with VPCs created
- Steps 2, 3, 4, 7, 8 fail with descriptive error messages
- VNI values are unique and monotonically increasing across all created VPCs

## Failure criteria

- Any valid CIDR is rejected
- Any invalid CIDR is accepted
- Overlapping CIDRs within the same org are accepted
- Non-overlapping CIDRs across different orgs are rejected
- Auto-allocation produces an overlapping CIDR
- VNI values are duplicated
