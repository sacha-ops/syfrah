# Test: 5 nodes with default zones — all unique

## Objective

5 nodes with default zones — all unique.

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

Wait until each node sees 4 active peer(s) (timeout: 30s).

### 2. Execute the test

```bash
syfrah fabric peers
```

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```


## Expected results

- all 5 zones are unique
- node-<value> region: default

## Failure criteria

- only <value> unique zones out of 5
- node-<value>: could not extract region
- node-<value> region: <value> (expected default)
