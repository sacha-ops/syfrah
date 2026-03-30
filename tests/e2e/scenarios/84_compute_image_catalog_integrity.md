# Test: Catalog integrity verification

## Objective

- catalog.json is valid JSON and has the expected schema
- Every image listed in the catalog has a corresponding .raw file on disk
- The base_url in the catalog points to a valid GitHub release
- The kernel entry exists and the vmlinux file is on disk

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Docker image built with real images from syfrah-images catalog

## Steps

### 1. Checking all catalog images have local files

```bash
stat -c%s "/opt/syfrah/images/${name}.raw"
```


### 2. Checking kernel entry

```bash
stat -c%s /opt/syfrah/vmlinux
```


## Expected results

- catalog.json is valid JSON
- catalog version is 1
- base_url points to syfrah-images repo
- image <value> exists on disk (<value>) MB)
- all <value> catalog images present on disk
- catalog has kernel entry (file=<value>)
- kernel version
- vmlinux on disk (<value>) KB)

## Failure criteria

- catalog.json is not valid JSON
- catalog version: '<value>' (expected 1)
- base_url unexpected
- image <value> on disk but too small (<value> bytes)
- image <value> in catalog but not on disk
- only <value>/<value> catalog images present
- catalog missing kernel entry
- kernel version missing
- vmlinux too small (<value> bytes)
- vmlinux not found on disk
