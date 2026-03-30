# Test: manual --region and --zone override the defaults

## Objective

manual --region and --zone override the defaults.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric status
```

```bash
syfrah fabric status
```


## Expected results

- node-1 has manual region: eu-west
- node-1 has manual zone: eu-west-paris-1
- node-2 has manual region: eu-central
- node-2 has manual zone: eu-central-frankfurt-1

## Failure criteria

- node-1 region override failed
- node-1 zone override failed
- node-2 region override failed
- node-2 zone override failed
