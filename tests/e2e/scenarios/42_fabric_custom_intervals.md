# Test: Daemon uses custom intervals from config.toml

## Objective

Daemon uses custom intervals from config.toml.

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

Wait until all 2 nodes see 1 peers each (timeout: 30s).

### 2. Waiting 45s for fast unreachable detection (20s timeout + 10s check + margin)

Wait 45 seconds.

```bash
syfrah fabric peers
```


## Expected results

- status shows custom health_check_interval
- status shows custom unreachable_timeout
- node-2 marked unreachable with fast config

## Failure criteria

- initial mesh did not converge
- status does not show custom health_check_interval
- status does not show custom unreachable_timeout
- node-2 not yet unreachable after 45s
