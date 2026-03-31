# E2E 271: Config-drive contains both SSH keys and network configuration

**Component**: compute / disk / cloud-init
**Issue**: #756

## Objective

Verify that when a VM is provisioned with both SSH keys and a structured
`CloudInitNetworkConfig`, the config-drive ISO contains all three files
(`meta-data`, `user-data`, `network-config`) and that the network-config
file contains valid Netplan v2 YAML with the correct IP, gateway, MTU,
and DNS settings.

## Preconditions

- `mkfs.vfat` (dosfstools) and `mcopy` (mtools) are installed.
- A base image is available for cloning.

## Steps

1. Create a `CloudInitConfig` with:
   - `hostname`: `"e2e-combined-vm"`
   - `ssh_authorized_keys`: one ed25519 public key
   - `network_config`: `CloudInitNetworkConfig { ip: "10.0.1.5", prefix_len: 24, gateway: "10.0.1.1", mtu: 1350, dns: ["8.8.8.8", "1.1.1.1"] }`
2. Call `generate_cloud_init()` to produce the config-drive image.
3. Extract all three files from the FAT32 image using `mcopy`.
4. Boot a VM with the config-drive attached.
5. SSH into the VM using the injected key.
6. Run `ip addr show` and verify the interface has `10.0.1.5/24`.
7. Run `ip route show default` and verify the gateway is `10.0.1.1`.
8. Run `cat /sys/class/net/*/mtu` and verify MTU is `1350`.
9. Run `resolvectl dns` or `cat /etc/resolv.conf` and verify DNS contains `8.8.8.8` and `1.1.1.1`.

## Expected results

- Config-drive image contains `meta-data`, `user-data`, and `network-config`.
- `user-data` begins with `#cloud-config` and includes the SSH key.
- `network-config` is valid Netplan v2 YAML with the specified IP/prefix, gateway, MTU, and DNS.
- The guest VM applies both the SSH key (login works) and the network config (correct IP, gateway, MTU, DNS).

## Automated coverage

Unit tests `combined_ssh_and_network` and `iso_contains_both_files` in
`layers/compute/src/disk.rs` cover steps 1-3. Full VM boot (steps 4-9)
requires KVM and is covered by the `e2e_kvm` integration test suite.
