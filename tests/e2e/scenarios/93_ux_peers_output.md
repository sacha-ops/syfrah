# Test: UX — peers command output validation

## Objective

UX — peers command output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (alpha):
```bash
syfrah fabric init --name test-mesh --node-name alpha --endpoint 172.20.0.10:51820
syfrah fabric start
```

On bravo:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name bravo --endpoint 172.20.0.11:51820
```

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Testing: peers no duplicates

- Verify: No duplicate entries in `syfrah fabric peers`

### 3. Testing: peers region/zone displayed

- Verify: assert_regions_displayed "e2e-ux-peers-1"
- Verify: assert_regions_displayed "e2e-ux-peers-2"

### 4. Testing: peers no epoch dates

- Verify: No epoch/1970 dates in output

### 5. Testing: peers names readable

```bash
syfrah fabric peers
```

```bash
syfrah fabric peers
```


### 6. Testing: peers with no mesh

```bash
syfrah fabric peers
```


## Expected results

- peer name 'bravo' fully displayed
- peer name 'alpha' fully displayed
- peers no mesh: suggests init/join

## Failure criteria

- peer name 'bravo' truncated or missing
- peer name 'alpha' truncated or missing
- peers no mesh: unclear message
