# Test: UX — help and version output validation

## Objective

UX — help and version output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Testing: syfrah --help

```bash
syfrah --help
```


### 2. Testing: syfrah fabric --help

```bash
syfrah fabric --help
```


### 3. Testing: syfrah --version

```bash
syfrah --version
```


### 4. Testing: syfrah state --help

```bash
syfrah state --help
```


## Expected results

- syfrah --help: lists fabric
- syfrah --help: lists state
- syfrah fabric --help: lists <value>
- syfrah --version: not empty
- syfrah --version: contains semver
- syfrah state --help: lists <value>

## Failure criteria

- syfrah --help: missing fabric
- syfrah --help: missing state
- syfrah fabric --help: missing <value>
- syfrah --version: empty output
- syfrah --version: no semver found
- syfrah state --help: missing <value>
