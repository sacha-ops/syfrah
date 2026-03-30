# Test: UX Flow — Nodes coming and going (churn)

## Objective

UX Flow — Nodes coming and going (churn).

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (churn-srv-1):
```bash
syfrah fabric init --name test-mesh --node-name churn-srv-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On churn-srv-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name churn-srv-2 --endpoint 172.20.0.11:51820
```

On churn-srv-3:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name churn-srv-3 --endpoint 172.20.0.12:51820
```

On churn-srv-3:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name churn-srv-3 --endpoint 172.20.0.12:51820
```

Wait until all 3 nodes see 2 peers each (timeout: 60s).

### 2. Setting up 3-node mesh

Wait 3 seconds.


### 3. Verifying initial mesh

Wait 2 seconds.

```bash
syfrah fabric leave --yes
```

```bash
syfrah fabric peers
```


### 4. Final validation

```bash
syfrah fabric peers
```

- Verify: No epoch/1970 dates in output

## Expected results

- initial 3-node mesh converged
- round <value>: node 3 sees <value> active peer(s)
- <value> sees <value> active peers (>= 2)

## Failure criteria

- initial convergence failed
- round <value>: node 3 has no active peers
- <value> sees <value> active peers (expected >= 2)
