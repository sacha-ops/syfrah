# Test: VPC CRUD operations

## Objective

- Create, list, get, and delete VPCs via the OrgStore API
- Validate VNI allocation (monotonically increasing from 100)
- Validate CIDR format enforcement
- Verify filtering by project and org owner

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing VPCs)

## Steps

### 1. Create a project-scoped VPC

```bash
syfrah vpc create default --project backend --org acme --cidr 10.1.0.0/16
```

Expected output:
```
VPC created: default
  CIDR: 10.1.0.0/16
  VNI:  100
```

### 2. Create a second VPC (different project)

```bash
syfrah vpc create frontend-vpc --project frontend --org acme --cidr 10.2.0.0/16
```

Expected: VNI is 101 (monotonically increasing).

### 3. Create a shared org-level VPC

```bash
syfrah vpc create monitoring --org acme --shared --cidr 10.100.0.0/16
```

Expected: VNI is 102. Shared flag is true.

### 4. List all VPCs

```bash
syfrah vpc list
```

Expected: all three VPCs appear (`default`, `frontend-vpc`, `monitoring`).

### 5. List VPCs by project

```bash
syfrah vpc list --project backend --org acme
```

Expected: only `default` appears.

### 6. List VPCs by org (shared only)

```bash
syfrah vpc list --org acme --shared
```

Expected: only `monitoring` appears.

### 7. Reject duplicate VPC name

```bash
syfrah vpc create default --project backend --org acme --cidr 10.3.0.0/16
```

Expected: error indicating VPC already exists. Exit code non-zero.

### 8. Reject invalid CIDR

```bash
syfrah vpc create bad-vpc --project backend --org acme --cidr not-a-cidr
syfrah vpc create bad-vpc --project backend --org acme --cidr 10.0.0/16
syfrah vpc create bad-vpc --project backend --org acme --cidr 10.0.0.0/33
```

Expected: each command returns an error about an invalid CIDR. Exit code non-zero.

### 9. Delete a VPC

```bash
syfrah vpc delete frontend-vpc
```

Expected: deletion succeeds. The VPC no longer appears in `syfrah vpc list`.

### 10. Delete non-existent VPC

```bash
syfrah vpc delete frontend-vpc
```

Expected: error indicating VPC not found. Exit code non-zero.

## Pass criteria

- VPC create/list/delete operations succeed as described
- VNI starts at 100 and increments by 1 per VPC
- Invalid CIDRs and duplicate names are rejected with clear error messages
- Filtering by project owner and org owner returns only matching VPCs
- Deleted VPCs do not appear in subsequent list calls
