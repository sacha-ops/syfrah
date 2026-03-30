# Test: Node leaves and rejoins — gets new WG keypair

## Objective

Node leaves and rejoins — gets new WG keypair.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
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

On node-3:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-3 --endpoint 172.20.0.12:51820
```

On node-3:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-3 --endpoint 172.20.0.12:51820
```

Wait until each node sees 2 active peer(s) (timeout: 30s).

### 2. Execute the test

```bash
syfrah fabric leave --yes
```

```bash
pkill -f syfrah
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: `syfrah fabric peers` shows 2 peer(s)
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- node-3 has new WG key after rejoin

## Failure criteria

- node-3 WG key unchanged after rejoin
- could not get mesh IPv6 (ipv6_1=<value>, ipv6_3=<value>)
