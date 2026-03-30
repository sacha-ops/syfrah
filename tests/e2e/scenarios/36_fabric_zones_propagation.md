# Test: region/zone propagates to other nodes via peer announcements

## Objective

region/zone propagates to other nodes via peer announcements.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric peers
```

- Verify: `syfrah fabric peers` shows 2 peer(s)

## Expected results

- node-3 sees node-1's region (dc-paris) via announcement
- region propagation test (field present in protocol)

## Failure criteria

- Any syfrah command returns a non-zero exit code unexpectedly
- Expected output patterns are missing
- Timeouts exceeded waiting for convergence
