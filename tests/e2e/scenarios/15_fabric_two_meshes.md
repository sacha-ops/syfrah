# Test: Two independent meshes on the same network

## Objective

Two independent meshes on the same network.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 4 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name alpha-1 --endpoint 172.20.0.10:51820
```

Start the peering service:
```bash
syfrah fabric start
```

Wait for the daemon to be responsive (up to 30s).

### 2. Execute the test

```bash
syfrah fabric peering start --pin "2222"
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: `syfrah fabric peers` shows 1 peer(s)
- Verify: Ping the peer mesh IPv6 address successfully
- Verify: assert_cannot_ping "e2e-alpha-1" "$beta2_ipv6"

## Expected results

- meshes have different secrets

## Failure criteria

- could not get mesh IPv6 for e2e-alpha-2
- could not get mesh IPv6 for e2e-beta-2
- meshes have same secret (should be impossible)
