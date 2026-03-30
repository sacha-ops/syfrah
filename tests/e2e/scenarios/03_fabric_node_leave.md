# Test: a node stops its daemon, remaining nodes continue

## Objective

- Node can be stopped cleanly
- Remaining nodes still have their interface and connectivity

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

Wait until each node sees 2 active peer(s) (timeout: 30s).

### 2. Stopping node-3 daemon

Wait 2 seconds.

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: The syfrah daemon is running
- Verify: `syfrah0` interface exists (`ip link show syfrah0`)
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- Node can be stopped cleanly
- Remaining nodes still have their interface and connectivity

## Failure criteria

- could not get mesh IPv6 for e2e-leave-2
