# Test: Stress test: 10 nodes join in 10 seconds, measure leader CPU/RAM

## Objective

Stress test: 10 nodes join in 10 seconds, measure leader CPU/RAM.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
```

Start the peering service:
```bash
syfrah fabric start
```

Wait for the daemon to be responsive (up to 30s).

### 2. Joining 9 nodes in rapid succession

Wait 1 seconds.

```bash
syfrah fabric join 172.20.0.10:51821 --node-name "node-N" --endpoint "172.20.0.N:51820" --pin "<PIN>"
```


### 3. Waiting for convergence

```bash
syfrah fabric peers
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: The syfrah daemon is running
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- join storm: <value> nodes converged in <value>s
- leader RSS after storm: <value>MB
- could not measure RSS

## Failure criteria

- join storm: mesh did not converge in 60s
- leader RSS after storm: <value>MB (high)
- could not get mesh IPv6 for e2e-storm-<value>
