# Test: UX Errors — init error messages

## Objective

UX Errors — init error messages.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name err-node-1 --endpoint 172.20.0.10:51820
```

### 2. Testing: init when mesh already exists

```bash
syfrah fabric init --name another-mesh --node-name err-node-2 --endpoint 172.20.0.10:51820
```


## Expected results

- double init: says already exists
- double init: suggests 'syfrah fabric leave'
- double init: no raw errors

## Failure criteria

- double init: unclear message
- double init: doesn't suggest leave command
- double init: contains raw error
