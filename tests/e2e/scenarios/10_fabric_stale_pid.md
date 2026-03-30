# Test: SIGKILL leaves stale PID file — restart must recover

## Objective

SIGKILL leaves stale PID file — restart must recover.

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

Wait until all 2 nodes see 1 peers each (timeout: 30s).

### 2. Killing node-1 daemon with SIGKILL

Wait 2 seconds.

```bash
pkill -9 -f syfrah
```

- Verify: assert_state_exists "e2e-pid-1"

### 3. Restarting from saved state

```bash
rm -f /root/.syfrah/control.sock
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
- Verify: `syfrah0` interface exists (`ip link show syfrah0`)
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- stale PID file exists after SIGKILL
- PID file already cleaned (acceptable)

## Failure criteria

- e2e-pid-1 did not see 1 peer within 30s
- could not get mesh IPv6 for e2e-pid-2
