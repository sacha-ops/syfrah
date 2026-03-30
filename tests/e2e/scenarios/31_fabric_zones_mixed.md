# Test: mix of auto and manual zones in the same mesh

## Objective

mix of auto and manual zones in the same mesh.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (node-1):
```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region custom-dc --zone custom-dc-rack1
syfrah fabric start
```

On node-3:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-3 --endpoint 172.20.0.12:51820
```

Wait until all 3 nodes see 2 peers each (timeout: 30s).

### 2. Execute the test

```bash
syfrah fabric status
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: `syfrah fabric peers` shows 2 peer(s)
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- node-2 custom region preserved in mesh

## Failure criteria

- could not extract region for e2e-zmix-2
- node-2 region: <value> (expected custom-dc)
- could not get mesh IPv6 for e2e-zmix-2
