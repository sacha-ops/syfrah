# Test: UX Flow — Add 5 nodes

## Objective

UX Flow — Add 5 nodes.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (scale-node-1):
```bash
syfrah fabric init --name test-mesh --node-name scale-node-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On scale-node-${i}:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name scale-node-${i} --endpoint 172.20.0.$((9 + i)):51820
```


### 2. Server $i: joining mesh

Wait 3 seconds.


### 3. Waiting for full convergence

```bash
syfrah fabric peers
```

- Verify: No duplicate entries in `syfrah fabric peers`
- Verify: assert_regions_displayed "$node"
- Verify: No epoch/1970 dates in output
- Verify: assert_peer_count "$node" "$EXPECTED_PEERS"

## Expected results

- all <value> nodes converged to <value> peers

## Failure criteria

- convergence timeout
