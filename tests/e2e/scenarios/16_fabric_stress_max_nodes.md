# Test: Stress test: maximum node count on a 2-vCPU runner

## Objective

Stress test: maximum node count on a 2-vCPU runner.

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


### 2. Joining $EXPECTED nodes

Wait 1 seconds.


### 3. Waiting for full convergence ($NODE_COUNT nodes, $EXPECTED peers each)

```bash
syfrah fabric peers
```


### 4. Checking leader memory

```bash
bash -c 'cat /proc/$(cat /root/.syfrah/daemon.pid)/status
```

```bash
wc -c /root/.syfrah/state.json
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- <value> nodes converged in <value>s
- leader RSS: <value>MB (under 100MB)
- could not measure RSS (daemon may have exited)
- state.json size: <value> bytes (under 50KB)

## Failure criteria

- mesh did not fully converge in 180s
- leader RSS: <value>MB (over 100MB)
- state.json size: <value> bytes (over 50KB)
- could not get mesh IPv6 for e2e-max-<value>
