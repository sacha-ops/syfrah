# Test: Kill half the nodes abruptly, remaining nodes survive

## Objective

Kill half the nodes abruptly, remaining nodes survive.

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

Wait until each node sees 5 active peer(s) (timeout: 30s).

### 2. Killing nodes 4, 5, 6

Wait 5 seconds.

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: The syfrah daemon is running
- Verify: Ping the peer mesh IPv6 address successfully
- Verify: assert_state_exists "e2e-down-1"
- Verify: assert_state_exists "e2e-down-2"
- Verify: assert_state_exists "e2e-down-3"

## Expected results

- All steps complete without errors
- All verification checks pass

## Failure criteria

- could not get mesh IPv6 (ipv6_2=<value>, ipv6_3=<value>)
