# Test: Stress test: large data transfer via WireGuard tunnel

## Objective

Stress test: large data transfer via WireGuard tunnel.

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

### 2. Execute the test

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully
- Verify: The syfrah daemon is running

## Expected results

- 100MB transfer completed in <value>s
- receiver got all 100MB (<value> bytes)
- 10 rapid transfers all succeeded

## Failure criteria

- could not get mesh IPv6 for e2e-stp-2
- 100MB transfer took <value>s (too slow)
- receiver got <value> bytes (expected <value>)
- <value>/10 rapid transfers failed
