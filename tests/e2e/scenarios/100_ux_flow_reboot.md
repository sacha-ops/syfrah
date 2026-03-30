# Test: UX Flow — Survive a reboot

## Objective

UX Flow — Survive a reboot.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (reboot-srv-1):
```bash
syfrah fabric init --name test-mesh --node-name reboot-srv-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On reboot-srv-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name reboot-srv-2 --endpoint 172.20.0.11:51820
```

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Setting up 2-node mesh

- Verify: `syfrah fabric peers` shows 1 peer(s)

### 3. Step 1: Restarting server 2

Restart the server (simulate a reboot).


### 4. Step 5: Server 2 peers after reboot

```bash
syfrah fabric peers
```


### 5. Step 6: Server 1 peers after server 2 reboot

```bash
syfrah fabric peers
```

- Verify: No duplicate entries in `syfrah fabric peers`
- Verify: No epoch/1970 dates in output

## Expected results

- e2e-flow-reboot-2 daemon socket exists after reboot
- server-2 sees server-1 after reboot
- server-1 sees server-2 after reboot

## Failure criteria

- e2e-flow-reboot-2 daemon socket missing after reboot
- server-2 lost server-1 after reboot
- server-1 lost server-2 after reboot
