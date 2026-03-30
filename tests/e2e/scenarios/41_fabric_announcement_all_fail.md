# Test: All announcements fail — mesh must still converge via reconciliation

## Objective

All announcements fail — mesh must still converge via reconciliation.

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

Wait until all 3 nodes see 2 peers each (timeout: 90s).

### 2. Waiting for reconciliation-based convergence (up to 90s)

```bash
syfrah fabric peers
```

- Verify: `syfrah fabric peers` shows 2 peer(s)

## Expected results

- all nodes converged

## Failure criteria

- convergence timed out
