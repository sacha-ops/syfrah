# Test: Security Group Rules — CRUD + default rules

## Objective

Validate SecurityGroupRule persistence: add, remove, list, list-by-sg, default rule creation, and validation (invalid ports, duplicates).

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with org layer active
- A VPC exists (e.g. `test-vpc` with CIDR `10.0.0.0/16`)

## Steps

### 1. Create a default security group and verify default rules

Create a default SG for the VPC. The system should auto-create:
- Ingress TCP 22 from VPC CIDR (SSH)
- Ingress ICMP from VPC CIDR (ping)

```bash
syfrah sg create default --vpc test-vpc
```

**Expected**: SG created. Two default rules are auto-generated.

```bash
syfrah sg rules list --sg default --vpc test-vpc
```

**Expected**: Two rules listed:
- `Ingress TCP 22-22 from 10.0.0.0/16 (Allow SSH from VPC CIDR)`
- `Ingress ICMP from 10.0.0.0/16 (Allow ICMP from VPC CIDR)`

### 2. Add a custom rule

```bash
syfrah sg rules add --sg default --vpc test-vpc --direction ingress --protocol tcp --port 443 --source 0.0.0.0/0 --description "Allow HTTPS from anywhere"
```

**Expected**: Rule created successfully.

```bash
syfrah sg rules list --sg default --vpc test-vpc
```

**Expected**: Three rules listed (2 defaults + 1 custom).

### 3. Add a rule with a port range

```bash
syfrah sg rules add --sg default --vpc test-vpc --direction ingress --protocol tcp --port 8000-9000 --source 10.0.0.0/16 --description "Allow dev ports from VPC"
```

**Expected**: Rule created with port range 8000-9000.

### 4. Reject invalid port range

```bash
syfrah sg rules add --sg default --vpc test-vpc --direction ingress --protocol tcp --port 0 --source 10.0.0.0/16
```

**Expected**: Error — port must be between 1 and 65535.

```bash
syfrah sg rules add --sg default --vpc test-vpc --direction ingress --protocol tcp --port 9000-80 --source 10.0.0.0/16
```

**Expected**: Error — from port must be <= to port.

### 5. Reject duplicate rule ID

Attempt to add a rule with the same ID as an existing rule.

**Expected**: Error — rule already exists.

### 6. Remove a rule

```bash
syfrah sg rules remove --rule <rule-id-from-step-2>
```

**Expected**: Rule removed.

```bash
syfrah sg rules list --sg default --vpc test-vpc
```

**Expected**: Three rules remain (2 defaults + 1 port-range rule).

### 7. Remove a non-existent rule

```bash
syfrah sg rules remove --rule nonexistent-rule-id
```

**Expected**: Error — rule not found.

### 8. Create a second SG and verify isolation

```bash
syfrah sg create web-sg --vpc test-vpc
syfrah sg rules add --sg web-sg --vpc test-vpc --direction ingress --protocol tcp --port 80 --source 0.0.0.0/0
```

```bash
syfrah sg rules list --sg web-sg --vpc test-vpc
```

**Expected**: Only the one custom rule (no defaults since this is not the default SG).

```bash
syfrah sg rules list --sg default --vpc test-vpc
```

**Expected**: The default SG rules are unchanged.

## Validation

- All CRUD operations succeed or fail as expected
- Default rules are auto-created for the default SG
- Port validation rejects invalid ranges
- Duplicate rule IDs are rejected
- Rules are isolated per security group
- Rules persist across list operations (backed by redb)
