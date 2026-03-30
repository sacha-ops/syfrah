# Test: Stress test for concurrent joins — zero delay, 10 nodes

## Objective

Stress test for concurrent joins — zero delay, 10 nodes.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.11:51820
```

Start the peering service:
```bash
syfrah fabric start
```

Wait for the daemon to be responsive (up to 60s).

### 2. Joining 9 nodes rapidly

Wait 1 seconds.

```bash
syfrah fabric join 172.20.0.11:51821 --node-name "node-N" --endpoint "172.20.0.N:51820" --pin "<PIN>"
```


### 3. Waiting for full convergence

```bash
syfrah fabric peers
```

```bash
cat /root/.syfrah/state.json
```


## Expected results

- all 10 nodes converged to 9 peers
- e2e-stress-join-<value>: no duplicate peers

## Failure criteria

- convergence timed out
- e2e-stress-join-<value>: <value> duplicate peer(s)
