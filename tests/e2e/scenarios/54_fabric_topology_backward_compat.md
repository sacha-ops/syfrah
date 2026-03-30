# Test: old state.json (without topology field) loads correctly

## Objective

old state.json (without topology field) loads correctly.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (node-1):
```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On node-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name node-2 --endpoint 172.20.0.11:51820
```

Wait until all 2 nodes see 1 peers each (timeout: 30s).

### 2. Execute the test

```bash
syfrah fabric topology
```

```bash
syfrah fabric topology --json
```

```bash
syfrah fabric peers
```

```bash
syfrah fabric stop
```

```bash
syfrah fabric topology
```


## Expected results

- topology shows default region for legacy nodes
- legacy node-1 visible in topology
- legacy node-2 visible in topology
- topology --json works for legacy nodes
- JSON shows default region for legacy nodes
- peers command works alongside topology for legacy state
- topology loads after stripping topology field from state

## Failure criteria

- topology fails with nodes that have no explicit region
- legacy node-1 missing from topology
- legacy node-2 missing from topology
- topology --json fails for legacy nodes
- JSON missing default region
- peers command broken for legacy state
- topology broken after stripping topology field
