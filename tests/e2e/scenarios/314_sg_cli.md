# Test: SG CLI — Create, List, Show, Delete Security Groups

## Objective

Verify that the `syfrah sg` CLI commands correctly create, list, show, and
delete security groups with proper validation and error handling.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Clean state (no existing organizations, projects, VPCs, or security groups)
- Daemon running (`syfrah fabric init ...`)

## Steps

### 1. Set up org, project, and VPC

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah vpc create prod-vpc --project backend --org acme --cidr 10.2.0.0/16
```

**Expected:** All commands succeed with confirmation messages.

### 2. Create a security group

```bash
syfrah sg create web-sg --vpc prod-vpc --description "Web tier firewall"
```

**Expected output:**
```
Security group created: web-sg
  VPC:          vpc-prod-vpc
  Description:  Web tier firewall
  State:        Active
  Created:      <date>
```

### 3. Create a second security group (no description)

```bash
syfrah sg create db-sg --vpc prod-vpc
```

**Expected output:**
```
Security group created: db-sg
  VPC:          vpc-prod-vpc
  Description:
  State:        Active
  Created:      <date>
```

### 4. List security groups

```bash
syfrah sg list --vpc prod-vpc
```

**Expected output (table format):**
```
NAME                 VPC                  DEFAULT  STATE      RULES  VMs  CREATED
----------------------------------------------------------------------------------------
web-sg               vpc-prod-vpc         no       Active     0      0    <date>
db-sg                vpc-prod-vpc         no       Active     0      0    <date>
```

### 5. List security groups (JSON output)

```bash
syfrah sg list --vpc prod-vpc --json
```

**Expected:** Valid JSON array with both security groups, each containing
`id`, `name`, `description`, `vpc_id`, `is_default`, `state`, `rules`,
`attached_vms`, `created_at`, and `updated_at` fields.

### 6. Show security group details

```bash
syfrah sg show web-sg
```

**Expected output:**
```
Security Group: web-sg
  ID:           sg-vpc-prod-vpc/web-sg
  VPC:          vpc-prod-vpc
  Description:  Web tier firewall
  Default:      no
  State:        Active
  Created:      <date>
  Updated:      <date>

Rules: (none)

Attached VMs: (none)
```

### 7. Show security group with --vpc

```bash
syfrah sg show web-sg --vpc prod-vpc
```

**Expected:** Same output as step 6.

### 8. Delete a security group

```bash
syfrah sg delete db-sg --yes
```

**Expected output:**
```
Security group 'db-sg' deleted.
```

### 9. Verify deletion

```bash
syfrah sg list --vpc prod-vpc
```

**Expected:** Only `web-sg` remains in the list.

### 10. Attempt to show deleted security group

```bash
syfrah sg show db-sg
```

**Expected:** Error: `security group 'db-sg' not found`

### 11. Duplicate name rejected

```bash
syfrah sg create web-sg --vpc prod-vpc
```

**Expected:** Error indicating the security group already exists.

### 12. Delete with confirmation prompt (abort)

```bash
echo "n" | syfrah sg delete web-sg
```

**Expected:** `Aborted.` and the security group is NOT deleted.

### 13. Delete with --yes flag

```bash
syfrah sg delete web-sg --yes
```

**Expected output:**
```
Security group 'web-sg' deleted.
```

### 14. Empty list

```bash
syfrah sg list --vpc prod-vpc
```

**Expected output:**
```
No security groups found.

Create one with: syfrah sg create <name> --vpc prod-vpc
```

### 15. Invalid VPC

```bash
syfrah sg create test-sg --vpc nonexistent
```

**Expected:** Error: VPC 'nonexistent' not found.

## Cleanup

```bash
syfrah vpc delete prod-vpc --org acme --yes
syfrah project delete backend --org acme --yes
syfrah org delete acme --yes
```
