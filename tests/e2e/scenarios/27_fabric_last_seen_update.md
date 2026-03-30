# Test: last_seen timestamp updates from WireGuard handshakes

## Objective

last_seen timestamp updates from WireGuard handshakes.

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

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Waiting for health check to update last_seen (polling up to 90s)

Wait 5 seconds.

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```


## Expected results

- peer has a WireGuard handshake timestamp
- peer has handshake after ping
- peer still active after health check (last_seen updated)
- handshake is recent (seconds ago)
- handshake present

## Failure criteria

- could not get mesh IPv6 for e2e-seen-2
- no handshake detected
- peer not active — last_seen may not have been updated
- could not extract handshake text from peers output
