# Test: Cold start — fresh server, no pre-installed images

## Objective

Cold start — fresh server, no pre-installed images.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name cold-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Step 1: Verify no images on fresh install

```bash
syfrah compute image list --json
```


### 3. Step 4: Verify image in local list

```bash
syfrah compute image list --json
```


### 4. Step 5: Create VM with pulled image

```bash
syfrah compute vm create --name cold-test --image alpine-3.20 --vcpus 1 --memory 256
```

- Verify: Wait until VM `cold-test` reaches `Running` phase (timeout 30s)

### 5. Step 6: Cleanup

```bash
syfrah compute vm delete cold-test --yes
```

```bash
syfrah compute image delete alpine-3.20 --yes
```


## Expected results

- No images on fresh install
- Catalog shows <value> images
- Image pull succeeded and verified in image list
- alpine-3.20 appears in local list
- VM reached Running on KVM-capable host
- VM creation correctly reported no KVM

## Failure criteria

- Expected empty image list, got
- Catalog is empty or unreachable after 3 attempts
- Image pull failed or image not found in list after 3 attempts
- alpine-3.20 not found after pull
- VM creation failed on KVM-capable host
- VM creation failed with unclear error
