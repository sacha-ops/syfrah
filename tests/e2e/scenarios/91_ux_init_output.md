# Test: UX — init command output validation

## Objective

UX — init command output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Testing: init happy path output

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.110:51820
```

- Verify: Output contains `node-1`
- Verify: Output contains `fd[0-9a-f]`
- Verify: Output contains `region\|zone`

### 2. Testing: init suggested commands

- Verify: `syfrah fabric status` runs without errors

### 3. Testing: init output no raw errors

- Verify: `syfrah fabric status` does not contain `anyhow`
- Verify: `syfrah fabric status` does not contain `os error`
- Verify: `syfrah fabric status` does not contain `stack backtrace`

### 4. Testing: double init

```bash
syfrah fabric init --name test-mesh2 --node-name node-2 --endpoint 172.20.0.110:51820
```


## Expected results

- init output does not leak secret
- init output contains node name
- init output contains IPv6 address
- init output contains region/zone
- double init says already exists
- double init suggests 'syfrah fabric leave'

## Failure criteria

- init should not show secret
- init output missing node name
- init output missing IPv6 (fd prefix)
- init output missing region/zone
- double init: unclear message
- double init doesn't suggest 'syfrah fabric leave'
