# Test: UX — peering command output validation

## Objective

UX — peering command output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (peer-node-1):
```bash
syfrah fabric init --name test-mesh --node-name peer-node-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On peer-node-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name peer-node-2 --endpoint 172.20.0.11:51820
```

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Testing: peering resulted in connected mesh

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```


### 3. Testing: no raw errors

- Verify: `syfrah fabric status` does not contain `anyhow`
- Verify: `syfrah fabric status` does not contain `os error`

## Expected results

- peering: peer visible after PIN join
- peering: initiator visible to joiner

## Failure criteria

- peering: peer not visible after PIN join
- peering: initiator not visible to joiner
