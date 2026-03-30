# Test: 5 nodes form a mesh

## Objective

- All 5 nodes join successfully
- Each node sees 4 peers
- End-to-end connectivity (node-1 ↔ node-5)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 5 servers with network connectivity on port 51820/UDP
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

On node-4:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-4 --endpoint 172.20.0.13:51820
```

On node-5:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-5 --endpoint 172.20.0.14:51820
```

Wait until all 5 nodes see 4 peers each (timeout: 45s).

### 2. Execute the test

```bash
syfrah fabric peers
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- all 5 nodes converged to 4 peers

## Failure criteria

- convergence timed out
- could not get mesh IPv6 (ipv6_1=<value>, ipv6_5=<value>)
