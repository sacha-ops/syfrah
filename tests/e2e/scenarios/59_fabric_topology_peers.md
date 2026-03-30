# Test: peers --topology shows grouped output by region/zone

## Objective

peers --topology shows grouped output by region/zone.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric peers --topology
```

```bash
syfrah fabric peers
```


## Expected results

- peers --topology shows eu-west region
- peers --topology shows us-east region
- peers --topology shows zone eu-west-1b
- peers --topology shows zone us-east-1a
- peers --topology shows node-us
- peers --topology shows node-eu-2
- peers --topology shows node count in region header
- peers --topology shows count in region header
- flat peers shows column headers
- flat peers shows all peers

## Failure criteria

- peers --topology missing eu-west
- peers --topology missing us-east
- peers --topology missing zone eu-west-1b
- peers --topology missing zone us-east-1a
- peers --topology missing node-us
- peers --topology missing node-eu-2
- peers --topology missing node count in region
- flat peers missing column headers
- flat peers missing some peers
