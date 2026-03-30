# Test: Multiple nodes join simultaneously (race condition test)

## Objective

Multiple nodes join simultaneously (race condition test).

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 5 servers with network connectivity on port 51820/UDP
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

### 2. Joining 4 nodes rapidly

Wait 1 seconds.

```bash
syfrah fabric join 172.20.0.10:51821 --node-name "node-N" --endpoint "172.20.0.N:51820" --pin "<PIN>"
```


### 3. Waiting for convergence

```bash
syfrah fabric peers
```


## Expected results

- all 5 nodes converged to 4 peers
- no duplicate peers in state

## Failure criteria

- convergence timed out
- <value> duplicate peer(s) found in state
