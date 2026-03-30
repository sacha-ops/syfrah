# Test: 2 regions, cross-region peering + topology display

## Objective

2 regions, cross-region peering + topology display.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric topology
```

```bash
syfrah fabric topology
```


## Expected results

- topology shows eu-west region
- topology shows us-east region
- topology header shows 2 regions
- topology shows zone eu-west-1a
- topology shows zone eu-west-1b
- topology shows zone us-east-1a
- eu-west leader sees us-east peer in topology
- us-east node sees eu-west in topology

## Failure criteria

- topology missing eu-west
- topology missing us-east
- topology header missing region count
- topology missing zone eu-west-1a
- topology missing zone eu-west-1b
- topology missing zone us-east-1a
- cross-region peer missing from topology
- us-east node missing eu-west in topology
