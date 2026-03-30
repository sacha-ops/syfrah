# Test: Announcement retry after temporary network failure

## Objective

Announcement retry after temporary network failure.

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

Wait until all 2 nodes see 1 peers each (timeout: 30s).

### 2. Joining node-3 while node-2 is blocked

Wait 2 seconds.


### 3. Waiting for node-2 to discover node-3

```bash
syfrah fabric peers
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- all nodes converged after announcement retry

## Failure criteria

- initial 2-node mesh did not converge
- convergence timed out
