# Test: unreachable peer recovers automatically when connectivity returns

## Objective

unreachable peer recovers automatically when connectivity returns.

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

### 2. Waiting for keepalive + health check recovery (polling up to 90s)

Wait 5 seconds.

```bash
syfrah fabric peers
```

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- e2e-recov-1 can ping <value>
- peer still in list after recovery

## Failure criteria

- could not get mesh IPv6 for e2e-recov-2
- peer missing from list
