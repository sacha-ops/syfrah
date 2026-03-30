# Test: UX Flow — Secret rotation

## Objective

UX Flow — Secret rotation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (rot-srv-1):
```bash
syfrah fabric init --name test-mesh --node-name rot-srv-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On rot-srv-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name rot-srv-2 --endpoint 172.20.0.11:51820
```

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Setting up 2-node mesh

```bash
syfrah fabric token
```

- Verify: `syfrah fabric peers` shows 1 peer(s)

### 3. Step 1: Stop daemon

Wait 2 seconds.


### 4. Step 2: Rotate secret

```bash
syfrah fabric rotate --yes
```


### 5. Step 4: Check new secret

```bash
syfrah fabric token
```

- Verify: `syfrah fabric status` does not contain `anyhow`
- Verify: `syfrah fabric status` does not contain `os error`

## Expected results

- initial secret captured: <value>...
- rotate: shows confirmation
- rotate: requires daemon running (clear message)
- token: shows secret after rotation flow

## Failure criteria

- could not capture initial secret
- rotate: unclear output
- token: no secret after rotation
