# Test: Image listing with real images from catalog

## Objective

- syfrah compute image list returns images
- alpine-3.20 appears in the list (pre-downloaded from catalog)
- JSON output is valid and contains expected fields
- The image file on disk is a real raw image (non-zero size)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Docker image built with real images from syfrah-images catalog
- Compute CLI (syfrah compute image list) must be implemented

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name image-list --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Checking that real alpine-3.20.raw exists

```bash
stat -c%s /opt/syfrah/images/alpine-3.20.raw
```

```bash
stat -c%s /opt/syfrah/vmlinux
```

```bash
jq '.images | length' /opt/syfrah/catalog.json
```


## Expected results

- alpine-3.20.raw is a real image (<value>) MB)
- vmlinux kernel is a real file (<value>) KB)
- alpine-3.20 appears in image list
- JSON output is valid
- JSON contains alpine-3.20 image entry
- catalog.json has <value> image(s)

## Failure criteria

- alpine-3.20.raw is missing or too small (<value> bytes)
- vmlinux is missing or too small (<value> bytes)
- alpine-3.20 not in image list
- JSON output is not valid JSON
- JSON missing alpine-3.20
- catalog.json has no images
- catalog.json not found in container
