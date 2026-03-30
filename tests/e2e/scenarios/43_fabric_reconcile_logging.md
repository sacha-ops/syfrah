# Test: Reconciliation recovers after WG interface is removed

## Objective

Reconciliation recovers after WG interface is removed.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
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


### 2. Execute the test

```bash
mkdir -p /root/.syfrah
```

```bash
sh -c 'cat > /root/.syfrah/config.toml << EOF
```


- Verify: `syfrah fabric peers` shows 1 peer(s)

## Expected results

- daemon still running after WG interface removal

## Failure criteria

- daemon crashed after WG interface removal
