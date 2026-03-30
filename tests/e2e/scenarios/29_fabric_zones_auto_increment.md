# Test: zone auto-increments as nodes join

## Objective

zone auto-increments as nodes join.

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

### 2. Execute the test

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```

```bash
cat /root/.syfrah/state.json
```

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```


## Expected results

- all 3 nodes have unique zones
- all 3 nodes share the same region

## Failure criteria

- could not extract zone for one or more nodes (z1=<value>, z2=<value>, z3=<value>)
- zone collision: <value>, <value>, <value>
- could not extract region for one or more nodes (r1=<value>, r2=<value>, r3=<value>)
- regions differ: <value>, <value>, <value>
