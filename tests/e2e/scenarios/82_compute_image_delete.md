# Test: Image deletion

## Objective

- An imported image can be deleted
- The deleted image no longer appears in list
- The raw file is removed from disk
- Deleting a non-existent image returns an error

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Docker image built with real images from syfrah-images catalog
- Compute CLI (syfrah compute image delete) must be implemented

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name image-delete --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Creating and importing disposable image

Wait 1 seconds.

- Verify: assert_image_exists "e2e-image-delete" "disposable"

### 3. Deleting disposable image

Wait 1 seconds.

- Verify: assert_image_gone "e2e-image-delete" "disposable"

## Expected results

- delete command succeeded
- disposable removed from image list
- deleting non-existent image fails as expected

## Failure criteria

- delete command failed
- disposable still in image list after delete
- deleting non-existent image did not fail
