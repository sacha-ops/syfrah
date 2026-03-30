# Test: region/zone survives daemon restart

## Objective

region/zone survives daemon restart.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric status
```

```bash
pkill -f syfrah
```

```bash
syfrah fabric status
```


## Expected results

- zone set before restart
- zone preserved after restart

## Failure criteria

- could not extract zone before restart
- zone before restart
- could not extract zone after restart
- zone after restart: <value> (expected my-region-zone-42)
