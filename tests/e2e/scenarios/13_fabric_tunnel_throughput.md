# Test: Measure WireGuard tunnel throughput between two nodes

## Objective

Measure WireGuard tunnel throughput between two nodes.

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

### 2. Starting receiver on node-2

Wait 2 seconds.


## Expected results

- 20MB transfer completed in <value>s (>= 2 MB/s)
- average RTT: <value>ms
- ping completed (could not parse RTT)

## Failure criteria

- could not get mesh IPv6 for e2e-tp-2
- 20MB transfer took <value>s (too slow)
