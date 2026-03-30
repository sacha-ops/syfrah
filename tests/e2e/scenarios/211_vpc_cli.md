# Test: VPC CLI — Create, List, Delete VPCs

## Objective

Verify that the `syfrah vpc` CLI commands correctly create, list, and delete
VPCs (both project-scoped and shared) with proper validation and error handling.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Clean state (no existing organizations, projects, or VPCs)

## Steps

### 1. Set up org and project

```bash
syfrah org create acme
syfrah project create backend --org acme
```

**Expected:** Both commands succeed with confirmation messages.

### 2. Create a project-scoped VPC

```bash
syfrah vpc create my-vpc --project backend --org acme --cidr 10.2.0.0/16
```

**Expected output:**
```
VPC created: my-vpc
  Org:      acme
  Project:  backend
  CIDR:     10.2.0.0/16
  VNI:      100
  Created:  <date>
```

### 3. Create a project-scoped VPC with default CIDR

```bash
syfrah vpc create default-vpc --project backend --org acme
```

**Expected output:**
```
VPC created: default-vpc
  Org:      acme
  Project:  backend
  CIDR:     10.1.0.0/16
  VNI:      101
  Created:  <date>
```

### 4. Create a shared (org-level) VPC

```bash
syfrah vpc create shared-net --org acme --shared --cidr 10.100.0.0/16
```

**Expected output:**
```
VPC created: shared-net
  Org:      acme
  Shared:   yes
  CIDR:     10.100.0.0/16
  VNI:      102
  Created:  <date>
```

### 5. Create a shared VPC with default CIDR

```bash
syfrah vpc create monitoring --org acme --shared
```

**Expected output includes:**
```
  CIDR:     10.100.0.0/16
```

### 6. Attempt to create a duplicate VPC

```bash
syfrah vpc create my-vpc --project backend --org acme
```

**Expected:** Exit code 1, error message: `vpc already exists: my-vpc`

### 7. Attempt to create a VPC in a non-existent org

```bash
syfrah vpc create test-vpc --project backend --org ghost
```

**Expected:** Exit code 1, error message: `org not found: ghost`

### 8. Attempt to create a VPC in a non-existent project

```bash
syfrah vpc create test-vpc --project nope --org acme
```

**Expected:** Exit code 1, error message: `project not found: nope in org acme`

### 9. Attempt to create a VPC without --project or --shared

```bash
syfrah vpc create orphan-vpc --org acme
```

**Expected:** Exit code 1, error message containing `--project is required`

### 10. List VPCs (table format)

```bash
syfrah vpc list --org acme
```

**Expected output:**
- Header row with NAME, CIDR, VNI, OWNER, SHARED, CREATED columns
- Rows for `my-vpc`, `default-vpc`, `shared-net`, and `monitoring`
- Shared VPCs show `yes` in SHARED column
- VNI values are unique and incrementing from 100

### 11. List VPCs filtered by project

```bash
syfrah vpc list --project backend --org acme
```

**Expected:** Only project-scoped VPCs (`my-vpc`, `default-vpc`), not shared ones

### 12. List VPCs (JSON format)

```bash
syfrah vpc list --org acme --json
```

**Expected:**
- Valid JSON array
- Each object has `name`, `cidr`, `vni`, `owner`, `shared`, `created_at` fields
- `vni` values are unique integers starting from 100

### 13. Delete a VPC (with --yes)

```bash
syfrah vpc delete default-vpc --org acme --yes
```

**Expected output:**
```
VPC 'default-vpc' deleted.
```

### 14. Verify deletion

```bash
syfrah vpc list --org acme --json
```

**Expected:** JSON array without `default-vpc`

### 15. Attempt to delete a non-existent VPC

```bash
syfrah vpc delete ghost-vpc --org acme --yes
```

**Expected:** Exit code 1, error message: `vpc 'ghost-vpc' not found in org 'acme'`

### 16. Clean up

```bash
syfrah vpc delete my-vpc --org acme --yes
syfrah vpc delete shared-net --org acme --yes
syfrah vpc delete monitoring --org acme --yes
syfrah project delete backend --org acme --yes
syfrah org delete acme --yes
```

**Expected:** All commands succeed with confirmation messages.

### 17. Verify empty state

```bash
syfrah vpc list
```

**Expected output:**
```
No VPCs found.
```

## Pass criteria

- All commands exit 0 on success, 1 on error
- VNI allocation is monotonically increasing starting at 100
- Project-scoped and shared VPCs are created correctly
- Validation rejects invalid names with actionable messages
- Duplicate VPC creation is prevented
- Deletion of non-existent VPC is rejected
- List shows correct table/JSON output with NAME, CIDR, VNI, OWNER, SHARED, CREATED columns
- Filtering by org and project works correctly
- State persists across commands (redb)
