# Test: Security Group Rule CLI — add-rule, remove-rule, rules

## Objective

- `syfrah sg add-rule` parses all arguments correctly and sends the request to the daemon
- `syfrah sg remove-rule` accepts an SG name and rule ID
- `syfrah sg rules` lists rules in table format and supports `--json`
- Port parsing handles single ports and ranges
- Validation rejects invalid protocols, directions, and port values

## Prerequisites

- Syfrah daemon running with at least one VPC and a security group created
- `syfrah` installed and in PATH

## Steps

### 1. Add an ingress rule with CIDR source

```bash
syfrah sg add-rule web-sg --direction ingress --protocol tcp --port 443 --source 0.0.0.0/0 --description "HTTPS access"
```

**Expected**: Rule added successfully. Output shows rule ID, direction (ingress), protocol (tcp), ports (443), source (0.0.0.0/0), and description.

### 2. Add an ingress rule with SG source

```bash
syfrah sg add-rule db-sg --direction ingress --protocol tcp --port 5432 --source-sg web-sg --priority 50
```

**Expected**: Rule added with source `sg:web-sg` and priority 50.

### 3. Add an egress rule with port range

```bash
syfrah sg add-rule app-sg --direction egress --protocol tcp --port 8000-9000 --source 10.0.0.0/8
```

**Expected**: Rule added with ports `8000-9000`.

### 4. Add a rule without explicit source (defaults to 0.0.0.0/0)

```bash
syfrah sg add-rule web-sg --direction ingress --protocol icmp
```

**Expected**: Rule added with source `0.0.0.0/0` and no port (ICMP does not use ports).

### 5. List rules in table format

```bash
syfrah sg rules web-sg
```

**Expected**: Table output with columns: ID, DIRECTION, PROTOCOL, PORTS, SOURCE, DESCRIPTION. All rules added in steps 1 and 4 are listed.

### 6. List rules in JSON format

```bash
syfrah sg rules web-sg --json
```

**Expected**: JSON array of rule objects with all fields populated.

### 7. Remove a rule

```bash
syfrah sg remove-rule web-sg --rule-id <rule-id-from-step-1>
```

**Expected**: Confirmation message that the rule was removed.

### 8. Verify rule was removed

```bash
syfrah sg rules web-sg
```

**Expected**: The rule from step 1 is no longer listed. Only the ICMP rule from step 4 remains.

## Validation tests (should fail gracefully)

### 9. Port with ICMP protocol rejected

```bash
syfrah sg add-rule web-sg --direction ingress --protocol icmp --port 443
```

**Expected**: Error: `--port is not valid with protocol 'icmp'`

### 10. Invalid direction rejected

```bash
syfrah sg add-rule web-sg --direction both --protocol tcp --port 80
```

**Expected**: Error mentioning invalid direction.

### 11. Invalid port range rejected

```bash
syfrah sg add-rule web-sg --direction ingress --protocol tcp --port 9000-8000
```

**Expected**: Error: port range start must be <= end.

### 12. Both --source and --source-sg rejected

```bash
syfrah sg add-rule web-sg --direction ingress --protocol tcp --port 80 --source 10.0.0.0/8 --source-sg other-sg
```

**Expected**: Error from clap: cannot use `--source` and `--source-sg` together.

## Pass criteria

- All add/remove/list commands execute without panics
- Table output is aligned and readable
- JSON output is valid JSON
- Validation errors are clear and actionable
- Port ranges parse correctly (single and range)
