# Test: default region/zone auto-generation

## Objective

default region/zone auto-generation.

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
syfrah fabric status
```

```bash
syfrah fabric peers
```


## Expected results

- node-1 has default region: default
- node-1 has default zone: zone-1
- peers output shows REGION column
- peers output shows ZONE column

## Failure criteria

- node-1 missing default region
- node-1 missing default zone
- peers output missing REGION column
- peers output missing ZONE column
