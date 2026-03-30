# Test: syfrah fabric peers displays region/zone columns

## Objective

syfrah fabric peers displays region/zone columns.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
# Scenario: syfrah fabric peers displays region/zone columns
```

```bash
syfrah fabric peers
```

```bash
syfrah fabric status
```

```bash
syfrah fabric peers
```


## Expected results

- peers output shows REGION column header
- peers output shows ZONE column header
- leader sees joiner's region (default)
- leader sees joiner's zone
- node-1 status shows its own region eu-west
- joiner sees leader's region (eu-west)
- joiner sees leader's zone (ew-zone-1)

## Failure criteria

- peers output missing REGION column header
- peers output missing ZONE column header
- leader does not see joiner's region
- leader does not see joiner's zone
- node-1 status missing region
- joiner does not see leader's region
- joiner does not see leader's zone
