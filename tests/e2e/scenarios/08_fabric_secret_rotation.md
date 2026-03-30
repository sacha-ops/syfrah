# Test: Full secret rotation flow

## Objective

Full secret rotation flow.

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
syfrah fabric stop
```

```bash
pkill -f syfrah
```

```bash
syfrah fabric rotate --yes
```

```bash
syfrah fabric leave --yes
```

- Verify: `syfrah fabric peers` shows 2 peer(s)

## Expected results

- secret rotated (different from original)
- peer list cleared after rotation
- node-2 has the new secret

## Failure criteria

- secret did not change
- peer list not cleared (has <value> peers)
- node-2 has wrong secret
