# Test: UX Flow — First-time mesh setup

## Objective

UX Flow — First-time mesh setup.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Step 1: Server 1 creates mesh

```bash
syfrah fabric init --name first-mesh --node-name server-1 --endpoint 172.20.0.10:51820
```


### 2. Step 2: Server 1 starts peering

Wait 2 seconds.

```bash
syfrah fabric peering start --pin "<PIN>"
```


### 3. Step 3: Server 2 joins with PIN

```bash
syfrah fabric join 172.20.0.10:51821 --node-name server-2 --endpoint 172.20.0.11:51820 --pin "<PIN>"
```

- Verify: Output contains `joined\|approved\|accepted`

### 4. Step 4: Verify bidirectional peer visibility

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```

- Verify: Output contains `server-2`
- Verify: Output contains `server-1`

### 5. Step 5: Region/zone displayed

- Verify: assert_regions_displayed "e2e-flow-first-1"
- Verify: assert_regions_displayed "e2e-flow-first-2"

### 6. Step 6: Mesh connectivity via IPv6

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

```bash
syfrah fabric status --json # extract mesh IPv6 address
```

- Verify: Ping the peer mesh IPv6 address successfully
- Verify: No duplicate entries in `syfrah fabric peers`
- Verify: No epoch/1970 dates in output

## Expected results

- init: secret not leaked
- join: approval confirmed
- server-1 sees server-2 in peers
- server-2 sees server-1 in peers

## Failure criteria

- init: should not show secret
- join: no approval shown
- server-1 doesn't see server-2
- server-2 doesn't see server-1
- could not get mesh IPv6 addresses
