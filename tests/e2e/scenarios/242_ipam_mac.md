# Test: IPAM MAC address derivation from IP

## Objective

- MAC addresses are deterministically derived from allocated IPs
- Format: `02:00:{o1:02x}:{o2:02x}:{o3:02x}:{o4:02x}`
- Each IP allocation record includes the derived MAC
- Different IPs produce different MACs

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with org layer initialized
- An org, project, environment, VPC, and subnet already exist

## Steps

### 1. Set up test environment

```bash
syfrah org create test-mac-org
syfrah project create test-mac-proj --org test-mac-org
syfrah env create test-mac-env --project test-mac-proj --org test-mac-org
syfrah subnet create test-mac-subnet --env test-mac-env --project test-mac-proj --org test-mac-org
```

### 2. Allocate IPs and verify MAC derivation

Create two VMs in the subnet to trigger IPAM allocation:

```bash
syfrah compute vm create --name mac-vm-1 --image alpine-3.20 \
  --subnet test-mac-subnet --project test-mac-proj --org test-mac-org \
  --vcpus 1 --memory 512

syfrah compute vm create --name mac-vm-2 --image alpine-3.20 \
  --subnet test-mac-subnet --project test-mac-proj --org test-mac-org \
  --vcpus 1 --memory 512
```

### 3. Verify MAC format and uniqueness

```bash
VM1_IP=$(syfrah compute vm get mac-vm-1 --project test-mac-proj --org test-mac-org --format json | jq -r '.ip')
VM1_MAC=$(syfrah compute vm get mac-vm-1 --project test-mac-proj --org test-mac-org --format json | jq -r '.mac')

VM2_IP=$(syfrah compute vm get mac-vm-2 --project test-mac-proj --org test-mac-org --format json | jq -r '.ip')
VM2_MAC=$(syfrah compute vm get mac-vm-2 --project test-mac-proj --org test-mac-org --format json | jq -r '.mac')
```

## Expected results

1. **MAC format**: both MACs match the regex `^02:00:[0-9a-f]{2}:[0-9a-f]{2}:[0-9a-f]{2}:[0-9a-f]{2}$`
2. **Deterministic derivation**: MAC is derived from IP octets:
   - If VM1 IP is `10.0.1.3`, its MAC must be `02:00:0a:00:01:03`
   - If VM2 IP is `10.0.1.4`, its MAC must be `02:00:0a:00:01:04`
3. **Uniqueness**: `VM1_MAC != VM2_MAC`
4. **Locally administered**: first byte is `02` (bit 1 of first octet set = locally administered, bit 0 clear = unicast)
5. **Stored in allocation**: the MAC appears in the `IpAllocation` record alongside the IP

## Failure criteria

- MAC does not start with `02:00:`
- MAC does not match the expected derivation from IP octets
- Two different IPs produce the same MAC
- Allocation record has an empty or missing MAC field
- MAC format does not match `XX:XX:XX:XX:XX:XX` (lowercase hex, colon-separated)

## Cleanup

```bash
syfrah compute vm delete mac-vm-1 --project test-mac-proj --org test-mac-org
syfrah compute vm delete mac-vm-2 --project test-mac-proj --org test-mac-org
syfrah subnet delete test-mac-subnet
syfrah env destroy test-mac-env --project test-mac-proj --org test-mac-org
syfrah project delete test-mac-proj --org test-mac-org
syfrah org delete test-mac-org
```
