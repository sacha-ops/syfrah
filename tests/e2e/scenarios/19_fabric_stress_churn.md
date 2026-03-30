# Test: Stress test: nodes repeatedly join and leave for 90 seconds

## Objective

Stress test: nodes repeatedly join and leave for 90 seconds.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
```

Start the peering service:
```bash
syfrah fabric start
```

Wait for the daemon to be responsive (up to 20s).

### 2. Running $CYCLES join/leave cycles

Wait 2 seconds.

```bash
syfrah fabric join 172.20.0.10:51821 --node-name "churn-2-c$cycle" --endpoint 172.20.0.11:51820 --pin "<PIN>"
```

```bash
syfrah fabric join 172.20.0.10:51821 --node-name "churn-3-c$cycle" --endpoint 172.20.0.12:51820 --pin "<PIN>"
```

```bash
syfrah fabric leave --yes
```

```bash
pkill -f syfrah
```

```bash
syfrah fabric leave --yes
```

```bash
pkill -f syfrah
```

```bash
rm -rf /root/.syfrah
```

```bash
rm -rf /root/.syfrah
```


### 3. Final join after churn

```bash
syfrah fabric join 172.20.0.10:51821 --node-name "churn-2-final" --endpoint 172.20.0.11:51820 --pin "<PIN>"
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

```bash
pgrep -c -f syfrah
```

- Verify: The syfrah daemon is running
- Verify: `syfrah0` interface exists (`ip link show syfrah0`)
- Verify: Ping the peer mesh IPv6 address successfully

## Expected results

- no zombie processes on churn node (<value> syfrah processes)
- stable node state.json is valid JSON

## Failure criteria

- could not get mesh IPv6 for e2e-churn-2
- <value> syfrah processes on churn node (expected <= 1)
- stable node state.json is invalid
