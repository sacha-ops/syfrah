# Test: UX Errors — no raw Rust errors in any output

## Objective

UX Errors — no raw Rust errors in any output.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name raw-node-1 --endpoint 172.20.0.10:51820
```

### 2. Testing: commands with no mesh

```bash
"syfrah fabric peers" "syfrah fabric status" "syfrah fabric stop" "syfrah fabric start" "syfrah fabric leave" "syfrah fabric token" "syfrah --help" "syfrah fabric --help"; do
```


### 3. Testing: commands with running mesh

```bash
"syfrah fabric peers" "syfrah fabric status" "syfrah fabric token"; do
```


### 4. Testing: join to bad target

```bash
sh -c "syfrah fabric join 10.99.99.99:51821 --node-name test --endpoint 172.20.0.10:51820"
```


### 5. Testing: double init

```bash
sh -c "syfrah fabric init --name test2 --node-name n2 --endpoint 172.20.0.10:51820"
```


## Expected results

- <value> (no mesh): clean output
- <value> (running): clean output
- join bad target: clean output
- double init: clean output

## Failure criteria

- <value> (no mesh): contains raw error
- <value> (running): contains raw error
- join bad target: contains raw error
- double init: contains raw error
