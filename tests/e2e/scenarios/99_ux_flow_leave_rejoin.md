# Test: UX Flow — Leave and rejoin

## Objective

UX Flow — Leave and rejoin.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (lr-server-1):
```bash
syfrah fabric init --name test-mesh --node-name lr-server-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On lr-server-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name lr-server-2 --endpoint 172.20.0.11:51820
```

On lr-server-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name lr-server-2 --endpoint 172.20.0.11:51820
```

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Setting up 2-node mesh

- Verify: `syfrah fabric peers` shows 1 peer(s)

### 3. Step 1: Server 2 leaves

Wait 2 seconds.

```bash
syfrah fabric leave --yes
```


### 4. Step 2: Verify clean state after leave

- Verify: assert_clean_state "e2e-flow-lr-2"

### 5. Step 4: Peers visible after rejoin

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```


### 6. Step 5: Correct peer counts

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```

- Verify: No epoch/1970 dates in output

## Expected results

- leave: clean message
- server-1 sees server-2 after rejoin
- server-2 sees server-1 after rejoin
- server-1 has <value> active peer(s)
- server-2 has <value> active peer(s)

## Failure criteria

- leave: unclear message
- server-1 doesn't see server-2 after rejoin
- server-2 doesn't see server-1 after rejoin
- server-1 has 0 active peers
- server-2 has 0 active peers
