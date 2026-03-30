# Test: Image inspection with real catalog images

## Objective

- Inspecting a known image returns metadata
- Metadata contains expected fields (name, arch, format)
- Inspecting a non-existent image returns an error

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Docker image built with real images from syfrah-images catalog
- Compute CLI (syfrah compute image inspect) must be implemented

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name image-inspect --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Execute the test

```bash
# - Compute CLI (syfrah compute image inspect) must be implemented
```


## Expected results

- inspect output is valid JSON
- inspect shows correct name
- inspect shows format=raw
- inspect shows non-zero size (<value> MB)
- inspecting non-existent image fails as expected

## Failure criteria

- inspect output is not valid JSON
- inspect name: '<value>' (expected alpine-3.20)
- inspect format: '<value>' (expected raw)
- inspect size_mb is 0 or missing
- inspecting non-existent image did not fail
