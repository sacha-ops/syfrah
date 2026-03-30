# Test: VPC auto-creation on first subnet

## Objective

- When the first subnet is created in a project with no VPC, a default VPC is auto-created
- The auto-created VPC has a valid /16 CIDR and a VNI >= 100
- Calling ensure_default_vpc a second time returns the same VPC (idempotent)
- Two different projects get different VNIs

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Org layer enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name vpc-auto --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create org and project

```bash
syfrah org create acme
syfrah project create backend --org acme
```

- Verify: org "acme" appears in `syfrah org list`
- Verify: project "backend" appears in `syfrah project list --org acme`

### 3. Create first subnet (triggers VPC auto-creation)

```bash
syfrah subnet create frontend --env production --project backend --org acme
```

- Verify: a VPC named "default" exists in `syfrah vpc list --project backend --org acme`
- Verify: the default VPC has a /16 CIDR (e.g. 10.0.0.0/16)
- Verify: the default VPC has a VNI >= 100

### 4. Create second subnet (reuses existing default VPC)

```bash
syfrah subnet create database --env production --project backend --org acme
```

- Verify: still only one VPC named "default" in `syfrah vpc list --project backend --org acme`
- Verify: the VPC VNI has not changed

### 5. Create a second project and trigger its own default VPC

```bash
syfrah project create frontend --org acme
syfrah subnet create web --env staging --project frontend --org acme
```

- Verify: project "frontend" has its own "default" VPC
- Verify: the VNI for frontend's default VPC differs from backend's default VPC

## Expected results

- Each project gets exactly one default VPC on first subnet creation
- VPCs are idempotent: second subnet does not create a second VPC
- VNIs are unique and monotonically increasing (>= 100)
- CIDRs are auto-allocated as /16 blocks

## Failure criteria

- More than one default VPC exists for a single project
- VNI is below 100
- Two projects share the same VNI
- Subnet creation fails because VPC auto-creation failed
