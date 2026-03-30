# Test: 10-node mesh convergence time measurement

## Objective

10-node mesh convergence time measurement.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (node-1):
```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On node-$i:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-$i --endpoint 172.20.0.N:51820
```

Wait until all 10 nodes see 9 peers each (timeout: 60s).

### 2. Waiting for full convergence

```bash
syfrah fabric peers
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- 10 nodes converged in <value>s
- convergence time under 45s threshold

## Failure criteria

- convergence took <value>s (threshold: 45s)
- mesh did not converge within 60s
- could not get mesh IPv6 for e2e-conv-10
