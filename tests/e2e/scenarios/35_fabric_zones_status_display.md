# Test: syfrah fabric status shows region and zone

## Objective

syfrah fabric status shows region and zone.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
```

### 2. Execute the test

```bash
# Scenario: syfrah fabric status shows region and zone
```

```bash
syfrah fabric status
```


## Expected results

- status shows Region field
- status shows zone field
- default region is default

## Failure criteria

- status missing Region field
- status missing zone field
- unexpected default region
