# 284 — SSH into VM

## Goal
Verify the full lifecycle: create org/project/env/subnet/VM with an SSH key,
obtain the allocated IP from output, SSH in, run a command, and verify output.

## Prerequisites
- Syfrah daemon running with fabric mesh initialized
- Image `alpine-3.20` available locally
- SSH key pair at `~/.ssh/id_ed25519` / `~/.ssh/id_ed25519.pub`
- No pre-existing org hierarchy (test creates everything)

## Steps

### 1. Create org hierarchy
```bash
syfrah org create acme
syfrah org project create backend --org acme
syfrah org env create production --project backend --org acme
syfrah org vpc create default --cidr 10.0.0.0/16 --env production --project backend --org acme
syfrah org subnet create frontend --cidr 10.0.1.0/24 --vpc default --env production --project backend --org acme
```
**Expected**: each command succeeds with confirmation output.

### 2. Create VM with SSH key
```bash
syfrah compute vm create --name ssh-test --image alpine-3.20 \
  --subnet frontend --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created, output includes allocated IP (e.g. `10.0.1.3`).

### 3. Verify VM is running
```bash
syfrah compute vm get ssh-test
```
**Expected**: phase = `Running`, IP field shows allocated address.

### 4. SSH into the VM
```bash
ssh -o StrictHostKeyChecking=no -o ConnectTimeout=30 root@10.0.1.3 "hostname"
```
**Expected**: prints the VM hostname (e.g. `ssh-test`).

### 5. Run a command inside the VM
```bash
ssh root@10.0.1.3 "uname -a && ip addr show eth0"
```
**Expected**:
- `uname` shows Linux kernel info
- `eth0` has IP `10.0.1.3/24`

### 6. Verify DNS resolution inside VM
```bash
ssh root@10.0.1.3 "cat /etc/resolv.conf"
```
**Expected**: nameserver entries for `8.8.8.8` and `1.1.1.1`.

### 7. Cleanup
```bash
syfrah compute vm delete ssh-test
```
**Expected**: VM deleted, TAP interface removed, IP released.

## Pass criteria
- SSH connection succeeds on first attempt (within 30s timeout)
- Command output is received correctly
- Network configuration inside VM matches expected values
- Cleanup leaves no orphaned resources
