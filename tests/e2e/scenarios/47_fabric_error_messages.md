# Test: CLI error messages are actionable

## Objective

CLI error messages are actionable.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
```

### 2. Testing: start without init

```bash
syfrah fabric start
```


### 3. Testing: double init

```bash
syfrah fabric init --name test2 --node-name node-2 --endpoint 172.20.0.10:51820
```


## Expected results

- start without init suggests init/join
- double init suggests leave

## Failure criteria

- start without init: unhelpful message
- double init: unhelpful message
