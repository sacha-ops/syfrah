# Test: Stress test: 12-node mesh, verify O(N²) announcements converge

## Objective

Stress test: 12-node mesh, verify O(N²) announcements converge.

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


### 2. Joining $EXPECTED nodes sequentially

Wait 1 seconds.


### 3. Waiting for announcement flood to settle (N*(N-1)/2 = $((NODE_COUNT * EXPECTED / 2)) announcements)

```bash
syfrah fabric peers
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- all <value> nodes converged in <value>s after joins
- all nodes see <value> peers

## Failure criteria

- did not converge in 90s
- some nodes have incomplete peer views
- could not get mesh IPv6 (ipv6_first=<value>, ipv6_last=<value>)
- could not get mesh IPv6 for mid-mesh check
