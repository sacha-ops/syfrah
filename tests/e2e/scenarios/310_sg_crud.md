# Test: Security Group CRUD operations

## Objective

- Verify security groups can be created, listed, retrieved, and deleted
- Verify a default SG is auto-created when a VPC is created
- Verify the default SG cannot be deleted
- Verify duplicate SG names within the same VPC are rejected
- Verify listing SGs filters correctly by VPC

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing orgs, VPCs, or security groups)

## Steps

### 1. Set up org, project, and VPC

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah vpc create prod-vpc --project backend --org acme --cidr 10.1.0.0/16
```

Expected: all succeed. VPC `prod-vpc` is created.

### 2. Verify default SG was auto-created

```bash
syfrah sg list --vpc prod-vpc
```

Expected output includes:
```
NAME     VPC         DEFAULT  STATE   DESCRIPTION
default  vpc-prod-vpc  true   Active  Default security group for VPC prod-vpc
```

### 3. Create a custom security group

```bash
syfrah sg create web --vpc prod-vpc --description "Web tier"
```

Expected:
```
Security group created: web
  VPC: prod-vpc
  Description: Web tier
```

### 4. Create a second custom SG

```bash
syfrah sg create backend --vpc prod-vpc --description "Backend services"
```

Expected: succeeds with SG name `backend`.

### 5. List SGs for the VPC

```bash
syfrah sg list --vpc prod-vpc
```

Expected: 3 security groups listed — `default`, `web`, `backend`.

### 6. Duplicate SG name rejected

```bash
syfrah sg create web --vpc prod-vpc
```

Expected: error — security group `web` already exists.

### 7. Delete a custom SG

```bash
syfrah sg delete web --vpc prod-vpc
```

Expected: succeeds. Listing SGs now shows 2 (`default`, `backend`).

### 8. Delete default SG rejected

```bash
syfrah sg delete default --vpc prod-vpc
```

Expected: error — cannot delete default security group.

### 9. SG not found

```bash
syfrah sg delete nonexistent --vpc prod-vpc
```

Expected: error — security group not found.

### 10. Create second VPC and verify isolated default SG

```bash
syfrah vpc create dev-vpc --project backend --org acme --cidr 10.2.0.0/16
syfrah sg list --vpc dev-vpc
```

Expected: `dev-vpc` has its own `default` SG. SGs from `prod-vpc` are not listed.

## Pass criteria

- All steps produce expected output
- Default SG is automatically created with every VPC
- Default SG is undeletable
- Duplicate SG names within a VPC are rejected
- SGs are correctly scoped to their VPC
