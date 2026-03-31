# Test: Config-drive network config v2

## Objective

- The cloud-init config-drive includes a `network-config` file when network parameters are provided
- The network-config file contains valid cloud-init network config v2 YAML
- The generated config includes the correct IP address, gateway, DNS servers, and MTU

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- `mkfs.vfat` (dosfstools) and `mcopy` (mtools) installed
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name netcfg-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Pull an image for the test

```bash
sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json'
```

```bash
syfrah compute image pull alpine-3.20
```

### 3. Create a VM with network configuration

```bash
syfrah compute vm create --name netcfg-test --image alpine-3.20 --vcpus 1 --memory 512
```

### 4. Verify the config-drive contains network-config

Locate the instance directory and extract the network-config from the config-drive image:

```bash
INSTANCE_DIR=$(ls -d /opt/syfrah/instances/*/cloud-init.img | head -1)
EXTRACT_DIR=$(mktemp -d)
mcopy -i "$INSTANCE_DIR" ::network-config "$EXTRACT_DIR/network-config"
cat "$EXTRACT_DIR/network-config"
```

**Expected**: The file exists and contains valid YAML with:
- `network.version` equals `2`
- `network.ethernets.eth0.addresses` contains an IP with prefix length
- `network.ethernets.eth0.gateway4` is set
- `network.ethernets.eth0.mtu` equals `1350`
- `network.ethernets.eth0.nameservers.addresses` contains `8.8.8.8` and `1.1.1.1`

### 5. Verify MTU value accounts for VXLAN + WireGuard overhead

```bash
grep 'mtu' "$EXTRACT_DIR/network-config"
```

**Expected**: MTU is 1350 (1500 - 50 VXLAN - 80 WireGuard = 1370, rounded down to 1350 for safety margin).

### 6. Cleanup

```bash
syfrah compute vm delete --name netcfg-test --force
rm -rf "$EXTRACT_DIR"
syfrah fabric leave --force
```

## Pass criteria

- The `network-config` file is present on the config-drive FAT32 image
- The YAML is valid cloud-init network config v2
- IP, gateway, DNS, and MTU fields are all populated correctly
- MTU is 1350
