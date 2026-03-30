# Test: UX — status command output validation

## Objective

UX — status command output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name status-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be responsive (up to 30s).

### 2. Testing: status shows visual sections

```bash
syfrah fabric status
```

- Verify: assert_output_contains "e2e-ux-status-1" "syfrah fabric status" "status-node"
- Verify: assert_output_matches "e2e-ux-status-1" "syfrah fabric status" "fd[0-9a-f]"

### 3. Testing: --show-secret reveals secret

```bash
syfrah fabric status --show-secret
```


### 4. Testing: --verbose shows config and metrics

```bash
syfrah fabric status --verbose
```


### 5. Testing: status after stop

Wait 2 seconds.

```bash
syfrah fabric status
```


### 6. Testing: status with no mesh

```bash
syfrah fabric status
```


## Expected results

- status shows Mesh section
- status shows Network section
- status shows Peers section
- status shows region/zone
- status masks secret (shows --show-secret hint)
- status does not leak full secret
- --show-secret reveals full secret
- config section hidden by default
- --verbose shows config section
- status shows daemon health
- status shows interface health
- status shows stopped state
- status after stop: no raw errors
- status no mesh: suggests init/join

## Failure criteria

- status missing Mesh section
- status missing Network section
- status missing Peers section
- status missing region/zone
- status does not mask secret
- status leaks full secret in default mode
- --show-secret did not reveal full secret
- config section visible without --verbose
- --verbose missing config section
- status missing daemon health
- status missing interface health
- status after stop: unclear
- status after stop: shows raw error
- status no mesh: unclear message
