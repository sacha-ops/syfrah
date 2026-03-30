# Test: Overlay — TAP and veth management

## Objective

Verify that TAP devices and veth pairs are correctly created, attached to
a VPC bridge, and cleaned up on deletion.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Root / `CAP_NET_ADMIN` privileges
- `iproute2` installed (provides `ip` command)
- No conflicting interfaces named `syftap-*` or `syfve-*`

## Steps

### 1. Create a VPC bridge

```bash
ip link add syfbr-100 type bridge
ip link set syfbr-100 up
```

Verify:
```bash
ip link show syfbr-100
```
Expected: bridge exists and is UP.

### 2. Create a TAP device for a VM

```bash
syfrah overlay tap create --vm-id test-vm-1
```

Verify:
```bash
ip link show syftap-test-vm-1
```
Expected: TAP device `syftap-test-vm-1` exists and is UP.

### 3. Attach TAP to bridge

```bash
syfrah overlay tap attach --vm-id test-vm-1 --bridge syfbr-100
```

Verify:
```bash
bridge link show dev syftap-test-vm-1
```
Expected: `syftap-test-vm-1` is listed as a port of `syfbr-100`.

### 4. Create a veth pair for a container

```bash
syfrah overlay veth create --vm-id test-ctr-1
```

Verify:
```bash
ip link show syfve-test-ctr-1-h
ip link show syfve-test-ctr-1-c
```
Expected: both ends exist and are UP.

### 5. Attach veth host side to bridge

```bash
syfrah overlay veth attach --vm-id test-ctr-1 --bridge syfbr-100
```

Verify:
```bash
bridge link show dev syfve-test-ctr-1-h
```
Expected: `syfve-test-ctr-1-h` is a port of `syfbr-100`.

### 6. Idempotency — re-create TAP

```bash
syfrah overlay tap create --vm-id test-vm-1
```

Expected: command succeeds, no error about device already existing.

### 7. Delete TAP

```bash
syfrah overlay tap delete --vm-id test-vm-1
```

Verify:
```bash
ip link show syftap-test-vm-1 2>&1
```
Expected: `Device "syftap-test-vm-1" does not exist.`

### 8. Delete veth pair

```bash
syfrah overlay veth delete --vm-id test-ctr-1
```

Verify:
```bash
ip link show syfve-test-ctr-1-h 2>&1
ip link show syfve-test-ctr-1-c 2>&1
```
Expected: both ends removed (deleting one end of a veth pair removes the peer).

### 9. Cleanup

```bash
ip link del syfbr-100 2>/dev/null || true
```

## Expected result

All TAP and veth operations succeed idempotently, interfaces are correctly
attached to the VPC bridge, and deletion cleans up all interfaces.
