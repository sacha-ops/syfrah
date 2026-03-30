# Test: VXLAN interface lifecycle

## Objective

- VXLAN interface is created with correct VNI, local IP, and flags
- VXLAN interface is attached to the VPC bridge after creation
- Idempotent creation does not duplicate interfaces
- VXLAN interface is cleanly deleted

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Root access or NET_ADMIN capability
- Kernel VXLAN module loaded (`modprobe vxlan`)
- A VPC bridge already exists (e.g. `syfbr-test-vpc`)

## Steps

### 1. Create a VPC bridge for testing

```bash
ip link add syfbr-test-vpc type bridge
ip link set syfbr-test-vpc up
```

### 2. Create VXLAN interface

```bash
syfrah overlay vxlan create --vpc test-vpc --vni 100 --local-ip fd00::1
```

Verify:
```bash
ip -d link show syfvx-test-vpc
```

Expected output contains:
- `vxlan id 100`
- `local fd00::1`
- `dstport 4789`
- `nolearning`
- `proxy`
- State: `UP`

### 3. Verify bridge attachment

```bash
ip link show syfvx-test-vpc | grep master
```

Expected: `master syfbr-test-vpc`

### 4. Idempotent creation

```bash
syfrah overlay vxlan create --vpc test-vpc --vni 100 --local-ip fd00::1
```

Should succeed without error. Verify only one `syfvx-test-vpc` interface exists:
```bash
ip link show | grep -c syfvx-test-vpc
```

Expected: `1` (not duplicated)

### 5. Delete VXLAN interface

```bash
syfrah overlay vxlan delete --vpc test-vpc
```

Verify:
```bash
ip link show syfvx-test-vpc 2>&1
```

Expected: `Device "syfvx-test-vpc" does not exist.`

### 6. Cleanup

```bash
ip link del syfbr-test-vpc 2>/dev/null
```

## Expected results

- VXLAN interface created with VNI 100, local fd00::1, port 4789, nolearning, proxy
- Interface attached to syfbr-test-vpc bridge
- Second creation is a no-op
- Deletion removes the interface cleanly

## Failure criteria

- VXLAN interface missing expected flags (nolearning, proxy)
- Interface not attached to bridge after creation
- Duplicate interfaces created on second invocation
- Deletion fails or leaves orphaned state
