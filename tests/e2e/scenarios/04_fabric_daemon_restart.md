# Test: a daemon restarts from saved state and reconnects

## Objective

- Daemon can start from saved state
- Peers are restored after restart
- Connectivity works after restart

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

### 2. Killing node-2 daemon process

Wait 2 seconds.

```bash
pkill -f syfrah
```


### 3. Restarting node-2 daemon

```bash
rm -f /root/.syfrah/control.sock /root/.syfrah/daemon.pid
```

```bash
ip link delete syfrah0
```

```bash
ls -la /root/.syfrah/
```

```bash
sh -c 'syfrah fabric start > /root/.syfrah/restart.log
```


### 4. restart.log output:

Wait 2 seconds.

```bash
cat /root/.syfrah/restart.log
```

```bash
cat /root/.syfrah/syfrah.log
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: The syfrah daemon is running
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- Daemon can start from saved state
- Peers are restored after restart
- Connectivity works after restart

## Failure criteria

- could not get mesh IPv6 for e2e-restart-1
