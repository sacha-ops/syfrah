# Networking Internals

This document describes the internal packet path, nftables architecture, and FDB propagation for Syfrah's overlay networking. It is intended for developers debugging cross-AZ connectivity issues.

## Packet path: cross-AZ VM-to-VM

```
VM-A (HV1, fsn1)                                    VM-B (HV2, nbg1)
     │                                                    ▲
     ▼                                                    │
  eth0 (container veth guest)                          eth0
     │                                                    ▲
     ▼                                                    │
  syfvh-{hash} (veth host, bridge port)            syfvh-{hash}
     │                                                    ▲
     ▼                                                    │
  syfb-{hash} (Linux bridge, VNI per VPC)          syfb-{hash}
     │                                                    ▲
     ▼                                                    │
  syfx-{hash} (VXLAN, nolearning+proxy)            syfx-{hash}
     │  FDB: dst MAC → remote VTEP IPv6                   ▲
     ▼  UDP/4789 encapsulation                            │  decapsulation
  syfrah0 (WireGuard)  ──── Internet ────  syfrah0 (WireGuard)
```

**Latency**: ~3ms cross-datacenter (Falkenstein ↔ Nuremberg via Hetzner).

## Interface naming

All interface names are ≤15 chars (Linux IFNAMSIZ limit). The suffix is a hash of the VPC VNI.

| Prefix | Type | Example | Description |
|--------|------|---------|-------------|
| `syfb-` | Bridge | `syfb-ef80f814` | One per VPC per hypervisor |
| `syfx-` | VXLAN | `syfx-ef80f814` | One per VPC per hypervisor, attached to bridge |
| `syfvh` | veth host | `syfvhc04d9852` | One per VM, attached to bridge |
| `syfvg` | veth guest | `syfvgc04d9852` | Renamed to `eth0` inside container netns |
| `syft-` | TAP | `syft-{hash}` | For Cloud Hypervisor VMs (KVM mode) |

## VXLAN configuration

The VXLAN interface is created with:
- `nolearning` — no MAC learning from incoming frames (static FDB only)
- `proxy` — kernel responds to ARP on behalf of remote VMs
- `proxy_arp=1` — sysctl on the VXLAN interface

This means:
- All remote VM MACs must be in the FDB as static entries
- All remote VM IPs must have ARP proxy entries (`ip neigh replace ... nud permanent`)
- The kernel answers ARP requests for remote VMs without flooding

## FDB propagation (how nodes learn about remote VMs)

FDB entries are propagated **bidirectionally** via Raft. Three layers of defense:

### 1. Real-time: PlacementEvent via Raft

When a VM is created on HV1:
1. `network_setup.rs` calls the `raft_placement_hook`
2. `PlaceVm` command is submitted to Raft
3. Raft replicates the log to ALL nodes
4. Each node's state machine emits `PlacementEvent::Added`
5. Each node's FDB listener (`daemon.rs`) calls `sync_placement()`:
   - If remote VM (hypervisor_id ≠ local): add FDB entry + ARP proxy
   - If local VM: backfill FDB for all remote peers in the same VPC

### 2. Cold rebuild at startup

At daemon boot, `rebuild_fdb()` iterates ALL placements from the Raft store and adds FDB + ARP entries for every remote VM. Handles reboot/restart.

### 3. Periodic reconciliation (every 30s)

The overlay reconciler compares expected FDB entries (from Raft) against actual kernel FDB entries. Adds missing, removes stale.

### 4. Lag recovery

If the PlacementEvent broadcast channel lags (>256 events), a full FDB rebuild is triggered automatically.

## nftables architecture

Two separate nftables tables, both with `policy drop`:

### Table 1: `inet syfrah` (L3/L4 filtering)

```
chain forward (priority 0, policy drop):
  1. udp dport 4789 drop              ← block VXLAN injection
  2. udp dport 51820 drop             ← block WireGuard injection
  3. tcp dport 51821 drop             ← block peering injection
  4. ct state established,related accept
  5. ct state invalid drop
  6. iifname "syfb-*" oifname "syfb-*" accept    ← same-bridge (intra-VPC)
  7. iifname "syfb-*" oifname "syfx-*" accept    ← bridge → VXLAN (outbound overlay)
  8. iifname "syfx-*" oifname "syfb-*" accept    ← VXLAN → bridge (inbound overlay)
  9. iifname "syfb-*" oifname != "syfb-*" accept ← internet egress
  10. [per-VM anti-spoofing + accept rules]
  11. [per-VM default deny]

chain input (priority 0, policy accept):
  [infrastructure port blocks for VM-sourced traffic]
```

**Important**: with `br_netfilter` enabled, bridged traffic also traverses the inet forward chain. The `iifname`/`oifname` are reported as the **bridge device** (not the bridge ports) by br_netfilter.

### Table 2: `bridge syfrah_sg` (Security Groups)

```
chain forward (priority 0, policy drop):
  1. ether type arp accept
  2. ct state established,related accept
  3. ct state invalid drop
  4. oifname != "lo" goto dispatch_ingress
  5. iifname != "lo" goto dispatch_egress

chain dispatch_ingress:
  oifname "syfvh{hash}" jump vm_{hash}_in    ← per-VM ingress chain

chain dispatch_egress:
  iifname "syfvh{hash}" jump vm_{hash}_out   ← per-VM egress chain

chain vm_{hash}_in:
  ct state established,related accept
  icmp type echo-request accept
  tcp dport 22 accept
  drop                                        ← default deny (MUST be last)

chain vm_{hash}_out:
  [egress rules]
  drop
```

### Rule ordering: accept BEFORE deny

Per-VM rules MUST have accept rules BEFORE the default deny drop:

```
CORRECT:                          WRONG (dead code):
  tcp dport 22 accept               oif {tap} drop        ← catches everything
  icmp echo-request accept          tcp dport 22 accept   ← never reached
  ct state established accept       icmp accept           ← never reached
  drop                  ← last     ct state accept        ← never reached
```

This was a bug fixed in PR #1278.

## br_netfilter interaction

`br_netfilter` (`bridge-nf-call-iptables=1`) makes bridged IPv4 packets traverse inet/ip nftables hooks. This has implications:

1. **Interface names**: In the inet forward chain, `iifname`/`oifname` for bridged packets are the **bridge device**, not the actual bridge ports. This is why `iifname "syfb-*" oifname "syfb-*" accept` works for same-bridge traffic.

2. **Conntrack namespaces**: With br_netfilter, conntrack tracking happens at the inet level, NOT the bridge level. This means `ct state established,related accept` in the bridge SG table may not see the connection state for bridged traffic. The inet table's conntrack rule handles this instead.

3. **Per-VM rules in inet**: Rules using `iif {tap}` / `oif {tap}` in the inet forward chain do NOT match bridged traffic because br_netfilter maps the interfaces to the bridge. These rules only apply to routed (non-bridged) forwarding.

## Testing cross-AZ ping

**Always ping from INSIDE the VM**, not from the host:

```bash
# WRONG — pings from bridge gateway IP, return path broken
ping 10.1.1.4

# CORRECT — ping from VM's network namespace
nsenter -t <container_pid> -n ping 10.1.1.4
```

The host's bridge gateway IP (10.1.1.1) has no FDB/ARP entry on remote nodes. Only VM IPs (assigned via IPAM) have entries. Pinging from the host produces VXLAN packets with the gateway IP as source — the remote node sees the request but the reply has nowhere to go because no FDB entry maps the gateway MAC back to the source VTEP.

## Debugging checklist

When cross-AZ ping fails:

### 1. WireGuard mesh
```bash
# Must work first — IPv6 ping between nodes
ping6 <remote_wg_ipv6>
```

### 2. FDB entries (bidirectional)
```bash
# On HV1: must have entry for HV2's VMs
bridge fdb show | grep dst
# Expected: 02:00:0a:01:01:04 dev syfx-* dst <HV2_WG_IPv6> self permanent

# On HV2: must have entry for HV1's VMs
bridge fdb show | grep dst
# Expected: 02:00:0a:01:01:03 dev syfx-* dst <HV1_WG_IPv6> self permanent
```

### 3. ARP proxy entries
```bash
# On HV1: must know HV2's VM MAC
ip neigh show dev syfx-*
# Expected: 10.1.1.4 lladdr 02:00:0a:01:01:04 PERMANENT

# On HV2: must know HV1's VM MAC
ip neigh show dev syfx-*
# Expected: 10.1.1.3 lladdr 02:00:0a:01:01:03 PERMANENT
```

### 4. VXLAN packet arrival
```bash
# On receiving node: tcpdump for VXLAN
tcpdump -i syfrah0 udp port 4789
# Should see: VXLAN, flags [I], vni 100, ICMP echo request
```

### 5. Bridge delivery
```bash
# On receiving node: tcpdump on bridge
tcpdump -i syfb-* icmp
# Should see both request AND reply
```

### 6. nftables
```bash
# Check per-VM SG chain — accept rules BEFORE drop
nft list chain bridge syfrah_sg vm_{hash}_in
# Must have: ct state accept, icmp accept, tcp 22 accept, THEN drop

# Check inet forward — bridge accept rules present
nft list chain inet syfrah forward | grep accept
# Must have: syfb-* ↔ syfx-* accept rules
```

### 7. Container namespace ping
```bash
# Find container PID
ps aux | grep init | grep -v grep
# Ping from inside
nsenter -t <pid> -n ping <remote_vm_ip>
```

## MAC address derivation

VM MAC addresses are derived from IP: `02:00:{hex(ip)}`. Example:
- IP `10.1.1.3` → MAC `02:00:0a:01:01:03`
- IP `10.1.1.4` → MAC `02:00:0a:01:01:04`

This ensures deterministic, collision-free MACs without coordination.
