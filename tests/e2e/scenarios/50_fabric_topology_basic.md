# Test: topology command shows tree after 3-node setup

## Objective

topology command shows tree after 3-node setup.

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


## Expected results

- topology shows mesh name
- topology shows region eu-west
- topology shows zone eu-west-1a
- topology shows node-1
- topology shows node-2
- topology shows node-3
- topology header shows 3 nodes
- topology shows default region for joiners

## Failure criteria

- topology missing mesh name
- topology missing region eu-west
- topology missing zone eu-west-1a
- topology missing node-1
- topology missing node-2
- topology missing node-3
- topology header missing node count
- topology missing default region
