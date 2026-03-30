# Test: redb/JSON consistency after concurrent joins

## Objective

redb/JSON consistency after concurrent joins.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
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

### 2. Execute the test

```bash
syfrah state get fabric peers
```

```bash
json_count=$(cat /root/.syfrah/state.json
```

```bash
if cat /root/.syfrah/state.json
```


## Expected results

- redb (<value>) and JSON (<value>) peer counts match
- state.json is valid JSON

## Failure criteria

- redb (<value>) and JSON (<value>) peer counts diverge
- state.json is invalid JSON
