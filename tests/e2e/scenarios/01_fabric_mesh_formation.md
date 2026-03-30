# Test: 3 nodes form a WireGuard mesh via CLI

## Objective

- All daemons start successfully
- Each node sees 2 peers
- syfrah0 interface exists on all nodes

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (node-1):
```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On node-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-2 --endpoint 172.20.0.11:51820
```

On node-3:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-3 --endpoint 172.20.0.12:51820
```

Wait until all 3 nodes see 2 peers each (timeout: 30s).

### 2. Execute the test

- Verify: The syfrah daemon is running
- Verify: `syfrah fabric peers` shows 2 peer(s)
- Verify: `syfrah0` interface exists (`ip link show syfrah0`)

## Expected results

- All daemons start successfully
- Each node sees 2 peers
- syfrah0 interface exists on all nodes

## Failure criteria

- Any syfrah command returns a non-zero exit code unexpectedly
- Expected output patterns are missing
- Timeouts exceeded waiting for convergence
