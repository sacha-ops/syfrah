# Test: Image import from local file

## Objective

- A raw disk file can be imported with a custom name
- The imported image appears in image list
- The imported image can be inspected

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Docker image built with real images from syfrah-images catalog
- Compute CLI (syfrah compute image import) must be implemented

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name image-import --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Importing test-disk.raw as custom-os

Wait 1 seconds.

- Verify: assert_image_exists "e2e-image-import" "custom-os"

## Expected results

- created 8 MB test raw file
- import command succeeded
- custom-os appears in image list
- duplicate import rejected

## Failure criteria

- failed to create test raw file
- import command failed
- custom-os not in image list
- duplicate import not rejected
