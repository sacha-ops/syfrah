# Test: reconciliation loop re-adds peers after WireGuard reset

## Objective

reconciliation loop re-adds peers after WireGuard reset.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
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

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Waiting for reconciliation loop (polling up to 60s)

Wait 5 seconds.

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- ping fails after WG peer removal (expected)
- ping still works (WG may have re-added via reconcile already)
- e2e-recon-1 can ping <value>

## Failure criteria

- could not get mesh IPv6 for e2e-recon-2
