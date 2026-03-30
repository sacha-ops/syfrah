# Test: UX Flow — Zero-interaction PIN onboarding

## Objective

UX Flow — Zero-interaction PIN onboarding.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Step 1: Server 1 creates mesh

```bash
syfrah fabric init --name pin-mesh --node-name pin-srv-1 --endpoint 172.20.0.10:51820
```


### 2. Step 2: Server 1 starts peering with auto PIN

Wait 2 seconds.

```bash
syfrah fabric peering start --pin "<PIN>"
```


### 3. Step 3: Server 2 joins with PIN (zero interaction)

```bash
syfrah fabric join 172.20.0.10:51821 --node-name pin-srv-2 --endpoint 172.20.0.11:51820 --pin "<PIN>"
```

- Verify: Output contains `joined\|approved\|accepted`

### 4. Step 4: Verify mesh formed

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```

- Verify: Output contains `pin-srv-2`
- Verify: Output contains `pin-srv-1`
- Verify: No duplicate entries in `syfrah fabric peers`
- Verify: No epoch/1970 dates in output

## Expected results

- PIN join: automatic approval (no interaction)
- server-1 sees server-2
- server-2 sees server-1

## Failure criteria

- PIN join: no auto-approval
- server-1 doesn't see server-2
- server-2 doesn't see server-1
