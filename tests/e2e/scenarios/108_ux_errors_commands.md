# Test: UX Errors — wrong command paths and suggestions

## Objective

UX Errors — wrong command paths and suggestions.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name cmd-node-1 --endpoint 172.20.0.10:51820
```

### 2. Testing: syfrah without subcommand

```bash
syfrah
```


### 3. Testing: syfrah with wrong subcommand

```bash
syfrah peering
```

```bash
syfrah init
```

```bash
syfrah join
```


### 4. Testing: suggested commands in error output are valid

- Verify: `syfrah fabric status` runs without errors

## Expected results

- syfrah bare: shows help/commands
- syfrah peering: shows help or error
- syfrah init: shows help or error
- syfrah join: shows help or error

## Failure criteria

- syfrah bare: no help
- syfrah peering: no guidance
- syfrah init: no guidance
- syfrah join: no guidance
