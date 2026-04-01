# ADR-001: Networking Roadmap — From Zero to SSH

**Status**: Accepted
**Date**: 2026-03-30
**Decided by**: Orchestrator + Sacha, after team debate on #711

## Context

The compute layer is implemented (17,740 LOC, VM/container lifecycle, image management, CLI). VMs have zero networking — no bridge, no IP, no NAT. An operator cannot SSH into a VM after creating it.

The core value proposition of Syfrah is: **turn rented dedicated servers from any provider into a unified programmable cloud with private networking across availability zones**. VXLAN over WireGuard is the mechanism that delivers this. It is not an optimization — it is the product.

The architecture docs specify: Org model (Org → Project → Environment), VPCs with VXLAN isolation, Subnets, security groups, IPAM, DNS. We implement the complete vision step by step. Every step is built with multi-node VXLAN in mind from day 1.

## Decision

### Step 1 — `syfrah-org` crate: organizational hierarchy

The foundation. Every resource belongs to an Org → Project → Environment.

**Types**:
- `Org { id, name, created_at }`
- `Project { id, name, org_id, created_at }`
- `Environment { id, name, project_id, ttl: Option<Duration>, deletion_protection: bool, labels: HashMap<String, String>, created_at }`

**Persistence**: redb tables — `orgs`, `projects`, `environments`.

**CLI**:
```bash
syfrah org create <name>
syfrah org list
syfrah org delete <name>

syfrah project create <name> --org <org>
syfrah project list [--org <org>]
syfrah project delete <name> --org <org>

syfrah env create <name> --project <project> --org <org> [--ttl 48h] [--deletion-protection] [--label key=value]
syfrah env list [--project <project>]
syfrah env destroy <name> --project <project> --org <org>
syfrah env extend <name> --ttl <duration>
```

**Validation**: names lowercase alphanumeric + hyphens + forward slashes, 3-63 chars, unique within parent.

### Step 2 — VPC: the network isolation unit

One VPC = one VXLAN VNI = one isolated L2 domain that spans all nodes.

**Type**:
```
Vpc {
    id: VpcId,
    name: String,
    cidr: Ipv4Net,          // e.g. 10.1.0.0/16
    vni: u32,               // VXLAN Network Identifier — unique per VPC
    owner: VpcOwner,        // Project(project_id) | Org(org_id)
    shared: bool,
    created_at: u64,
}

enum VpcOwner {
    Project(ProjectId),
    Org(OrgId),
}
```

**VNI allocation**: monotonically increasing counter starting at 100. Persisted in redb. Each VPC gets a unique VNI — this is the isolation boundary.

**3 ownership modes**:
- **Project VPC** (default): belongs to one project, auto-created with the first subnet
- **Shared VPC**: belongs to an org, attachable to N projects
- **Dedicated VPC**: explicitly created by the operator

**Persistence**: redb tables — `vpcs`, `vpc_attachments`, `vni_counter`.

**CLI**:
```bash
syfrah vpc create <name> --project <project> --org <org> [--cidr 10.2.0.0/16]
syfrah vpc create <name> --org <org> --shared [--cidr 10.100.0.0/16]
syfrah vpc list [--project <project>] [--org <org>]
syfrah vpc delete <name>
syfrah vpc attach <vpc> --project <project>
syfrah vpc detach <vpc> --project <project>
```

### Step 3 — Subnet: the IP allocation unit

An environment can have **N subnets**. Each subnet is a `/24` (or operator-specified) within its VPC's CIDR. VMs are placed into a specific subnet.

**Type**:
```
Subnet {
    id: SubnetId,
    name: String,
    vpc_id: VpcId,
    env_id: EnvironmentId,
    cidr: Ipv4Net,          // e.g. 10.1.1.0/24
    gateway: Ipv4Addr,      // .1
    created_at: u64,
}
```

**Creation**: explicit by the operator. If no `--cidr` is given, auto-allocate the next available `/24` within the VPC's CIDR.

**Persistence**: redb table — `subnets`.

**CLI**:
```bash
syfrah subnet create <name> --env <env> --project <project> --org <org> [--vpc <vpc>] [--cidr 10.1.1.0/24]
syfrah subnet list [--env <env>] [--vpc <vpc>]
syfrah subnet delete <name>
```

### Step 4 — VPC Peering

Connects two VPCs so their VMs can communicate. Enables shared VPC and hub & spoke topologies.

**Type**:
```
VpcPeering {
    id: PeeringId,
    vpc_a: VpcId,
    vpc_b: VpcId,
    status: PeeringStatus,  // Active | Pending | Deleted
    created_at: u64,
}
```

**Implementation on single node**: veth pair connects the two VPC bridges. Routes added for the peer's CIDR.

**Implementation on multi-node**: VXLAN already spans nodes. Peered VPCs add cross-VNI forwarding rules — the local bridge for VPC-A gets a route to VPC-B's CIDR via VPC-B's bridge on the same node. On remote nodes, VXLAN carries the traffic per-VNI.

**Hub & spoke**: hub peers with each spoke. No spoke-to-spoke peering = no route between spokes.

**Persistence**: redb table — `vpc_peerings`.

**CLI**:
```bash
syfrah vpc peer --from <vpc-a> --to <vpc-b>
syfrah vpc unpeer --from <vpc-a> --to <vpc-b>
syfrah vpc peerings [--vpc <vpc>]
```

### Step 5 — IPAM

Centralized IP allocator with full allocation tracking.

**Design**:
- Bitmap: 1 bit per IP in the subnet. For a `/24`: 256 bits = 32 bytes.
- Reserved: `.0` (network), `.1` (gateway), `.2` (reserved/DNS), `.255` (broadcast)
- Available: `.3` to `.254` (252 addresses per `/24`)
- MAC derived deterministically from IP: `02:00:{IP octets in hex}` (e.g. `10.0.1.5` → `02:00:0a:00:01:05`). No MAC allocation service needed. No conflicts within the current single-NIC IPv4 model (multi-NIC or dual-stack would require revisiting this scheme).
- Allocation is atomic (redb transaction)
- Release on VM delete

**IP allocation table**: separate from the bitmap, tracks the full lifecycle of each allocation:
```
IpAllocation {
    ip: Ipv4Addr,
    subnet_id: SubnetId,
    vm_id: Option<VmId>,        // None if allocated but VM not yet created
    mac: MacAddr,
    state: AllocationState,     // Reserved | Assigned | Orphaned
    allocated_at: u64,
    assigned_at: Option<u64>,   // when VM was actually created
}
```

This distinction matters: an IP can be allocated (bitmap bit set) but the VM creation may fail between IPAM allocation and VM boot. The `ip_allocations` table is the source of truth for what IPs are in use and why. The bitmap is the fast-path for allocation; the table is the audit trail. Orphaned allocations (allocated but no VM) are detected by the reconciliation loop and reclaimed.

**Persistence**: redb tables — `ipam_bitmaps` (subnet_id → bitmap), `ip_allocations` (ip + subnet_id → allocation record).

**No CLI** — consumed internally by compute during `vm create`.

### Step 6 — Network primitives (`syfrah-overlay` crate)

The Linux networking plumbing. All operations are idempotent. Designed for multi-node from day 1.

**`vxlan.rs`**: one VXLAN interface per VPC per node.
```
syfvx-{vpc_id}  →  VNI = vpc.vni, UDP port 4789
                    local IP = node's fabric IPv6 (syfrah0 address)
                    attached to syfbr-{vpc_id}
```
- Created on-demand when the first VM in a VPC lands on this node
- Remote VTEP = other nodes' fabric IPv6 addresses (from fabric peer list)
- On single node: VXLAN interface exists but all traffic is local — bridge FDB resolves local MACs directly without VXLAN encap/decap. The overlay stack is present in the datapath but adds negligible same-node overhead.
- On multi-node: VXLAN carries tenant traffic between nodes over the WireGuard fabric

**`fdb.rs`**: static Forwarding Database management.
- When a VM is created: announce its MAC + hosting node to all nodes in the VPC
- Each node adds a static FDB entry: `bridge fdb add {mac} dev syfvx-{vpc_id} dst {remote_node_fabric_ipv6}`
- No flood-and-learn. No broadcast. The control plane knows where every VM is.
- ARP proxy: VXLAN in proxy mode, neighbor entries populated from IPAM

**`bridge.rs`**: one Linux bridge per VPC per node.
```
syfbr-{vpc_id}  →  VXLAN interface attached
                    TAP/veth devices attached
                    Gateway IPs for local subnets
```

Gateway IP handling for multi-subnet on a single bridge: each subnet whose VMs are present on this node adds its gateway IP to the bridge (e.g. `10.1.1.1/24` and `10.1.2.1/24` on `syfbr-100`). Linux routes between subnets via the bridge's own IP stack. This is the distributed router model described in the overlay README. Careful attention required:
- Gateway IPs are added/removed as subnets gain/lose VMs on this node
- ARP proxy must answer for all gateway IPs
- nftables controls which subnets can communicate (default: isolated)

**`tap.rs`**: one TAP per VM (Cloud Hypervisor), one veth pair per container (crun).
```
TAP:  syftap-{vm_id}  →  attached to syfbr-{vpc_id}
veth: syfve-{vm_id}   →  one end in bridge, other end in container netns
```

**`veth_peer.rs`**: veth pair between two VPC bridges for peering (same node). Cross-node peering routes through VXLAN.

**`nft.rs`**: all firewall rules via nftables.
- **Anti-spoofing** per TAP: source MAC and IP must match IPAM-assigned values. Enforced before bridge — VM cannot impersonate another.
- **Default-deny ingress**: only SSH (TCP 22) and ICMP allowed inbound initially
- **Default-allow egress**: all outbound permitted
- **Conntrack**: established/related connections auto-allowed (stateful)
- **Subnet isolation**: VMs in different subnets within the same VPC cannot communicate unless explicitly allowed by security group rules
- **VPC isolation**: different VNIs = separate L2 domains. nftables blocks any cross-bridge forwarding unless VPCs are peered.
- **Peering rules**: FORWARD allowed between peered VPC bridges
- **SNAT masquerade**: outbound internet traffic NATed through node's public IP

**`NetworkBackend` trait**: for testing.
```rust
trait NetworkBackend: Send + Sync {
    // VXLAN
    fn create_vxlan(&self, name: &str, vni: u32, local_ip: Ipv6Addr, port: u16) -> Result<()>;
    fn delete_vxlan(&self, name: &str) -> Result<()>;
    fn add_fdb_entry(&self, bridge: &str, mac: MacAddr, vtep: Ipv6Addr) -> Result<()>;
    fn remove_fdb_entry(&self, bridge: &str, mac: MacAddr) -> Result<()>;
    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: MacAddr) -> Result<()>;
    // Bridge
    fn create_bridge(&self, name: &str) -> Result<()>;
    fn add_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr, prefix_len: u8) -> Result<()>;
    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<()>;
    fn delete_bridge(&self, name: &str) -> Result<()>;
    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()>;
    // TAP/veth
    fn create_tap(&self, name: &str) -> Result<()>;
    fn delete_tap(&self, name: &str) -> Result<()>;
    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()>;
    // Firewall
    fn apply_vm_rules(&self, tap: &str, mac: MacAddr, ip: Ipv4Addr) -> Result<()>;
    fn remove_vm_rules(&self, tap: &str) -> Result<()>;
    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<()>;
    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()>;
}
```

### Step 7 — FDB distribution

When a VM is created or deleted, all nodes in the VPC must update their FDB and ARP proxy tables.

**Mechanism**: fabric peer announcement channel (already exists for WireGuard peer distribution). Extend it with VM placement announcements:
```
VmPlacement {
    vpc_id: VpcId,
    vm_id: VmId,
    vm_mac: MacAddr,
    vm_ip: Ipv4Addr,
    subnet_id: SubnetId,
    hypervisor_id: FabricIpv6,   // which node the VM is on
    action: Add | Remove,
}
```

**On single node**: FDB entries point to local bridge (no VTEP needed). Announcements are stored but no remote distribution occurs.

**On multi-node**: each node receives the announcement and:
1. Adds FDB entry: `bridge fdb add {mac} dev syfvx-{vpc_id} dst {hypervisor_id_ipv6}`
2. Adds ARP proxy: `ip neigh add {vm_ip} lladdr {mac} dev syfvx-{vpc_id} nud permanent`

**Persistence**: redb table — `vm_placements` (vpc_id + vm_id → node, mac, ip, subnet). Rebuilt from fabric announcements on restart.

### Step 8 — Config-drive network injection

Extend the existing cloud-init config-drive (`disk.rs`) to include network config v2:

```yaml
network:
  version: 2
  ethernets:
    eth0:
      addresses:
        - 10.0.1.5/24
      gateway4: 10.0.1.1
      nameservers:
        addresses:
          - 8.8.8.8
          - 1.1.1.1
      mtu: 1350
```

**MTU 1350**: accounts for VXLAN (50 bytes) + WireGuard (80 bytes) overhead over standard 1500 MTU. Set from day 1 so VMs never hit MTU issues when multi-node traffic begins. On single node, the lower MTU has negligible impact on interactive traffic (SSH, API calls) and ~10% impact on bulk transfers — an acceptable tradeoff for operational stability.

### Step 9 — Compute integration

Wire networking into the VM lifecycle.

**`vm create --name web-1 --image alpine-3.20 --subnet frontend --project backend --org acme --ssh-key ~/.ssh/id.pub`**:

1. Resolve: `--subnet frontend` → subnet record → VPC (with VNI)
2. IPAM: allocate IP from subnet bitmap, derive MAC, create `IpAllocation{state: Reserved}`
3. VXLAN: ensure VPC VXLAN interface exists on this node (create if first VM in this VPC)
4. Bridge: ensure VPC bridge exists, VXLAN attached, subnet gateway IP on bridge
5. TAP: create TAP/veth, attach to bridge
6. FDB: add local FDB entry. Store `VmPlacement`. Announce to all fabric peers.
7. nftables: apply anti-spoofing, ingress deny, egress allow, NAT
8. Config-drive: generate with IP/gateway/DNS/MTU=1350
9. Boot VM
10. Update `IpAllocation{state: Assigned, assigned_at: now}`
11. Return IP to user

If any step between 2 and 9 fails, the cleanup path releases the IP allocation (marks as orphaned for reclamation) and removes any partially created network resources.

**`vm delete`**:
1. Stop VM
2. Announce `VmPlacement{Remove}` to all fabric peers
3. Remove FDB + ARP proxy entries on all nodes
4. Release IP (IPAM bitmap + delete `IpAllocation` record)
5. Delete TAP
6. Remove nftables rules
7. If bridge has no more TAPs: remove subnet gateway IP. If no gateway IPs remain: delete bridge + VXLAN + NAT rules.

**`vm list` / `vm get`**: display IP, subnet, VPC, node.

**Default behavior**: if `--subnet` is omitted and the environment has exactly one subnet, use it. If multiple, error: "specify --subnet".

### Step 10 — Cleanup + reconciliation

**Two levels of reconciliation**:
1. **Event-driven immediate**: every `vm create` / `vm delete` / `vm stop` triggers immediate cleanup of the affected resources
2. **Periodic safety reconcile** (every 30s): catches anything the event-driven path missed (crash between steps, partial failures)

**Daemon restart**:
- Scan existing bridges (`syfbr-*`), VXLAN (`syfvx-*`), TAPs (`syftap-*`), veth peers (`syfpeer-*`)
- Compare with redb state
- Re-apply nftables rules (they don't survive reboot)
- Re-populate FDB entries from `vm_placements` table
- Orphaned interfaces (in kernel but not in redb): log warning, delete
- Missing interfaces (in redb but not in kernel): re-create
- Orphaned IP allocations (`IpAllocation{state: Reserved}` older than 5 minutes with no corresponding VM): reclaim

**Bridge lifecycle**: created on first VM in VPC on this node, deleted on last VM out.

## Packet flow

### Same node, same subnet
```
VM-A (10.1.1.3) → tap-A → bridge (local FDB lookup, no VXLAN) → tap-B → VM-B (10.1.1.5)
```
Bridge resolves the destination MAC locally. VXLAN interface is attached but not involved.

### Same node, different subnets (same VPC)
```
VM-A (10.1.1.3) → tap-A → bridge → L3 routing via bridge gateway IPs → tap-B → VM-B (10.1.2.3)
```
Bridge holds gateway IPs for both subnets. Routes at L3. Filtered by nftables subnet isolation rules.

### Different nodes, same VPC
```
Node 1: VM-A (10.1.1.3) → tap-A → bridge → FDB: "dst MAC is on Node 2"
         → VXLAN encap (VNI=100, outer dst=Node2 fabric IPv6)
         → syfrah0 (WireGuard encrypt)
         → internet

Node 2: internet → syfrah0 (WireGuard decrypt)
         → VXLAN decap (VNI=100)
         → bridge → tap-B → VM-B (10.1.1.5)
```
Double encapsulation: VXLAN inside WireGuard. Total overhead: ~130 bytes/packet.

### Internet egress
```
VM (10.1.1.3) → tap → bridge (gateway 10.1.1.1) → nftables SNAT → node public IP → internet
```

### Peered VPCs (same node)
```
VM-A in VPC-1 (10.1.1.3) → bridge-1 → veth-peer → bridge-2 → VM-B in VPC-2 (10.2.1.3)
```

### Peered VPCs (different nodes)
```
Node 1: VM-A in VPC-1 → bridge-1 → route to VPC-2 CIDR → veth-peer → bridge-2
         → VXLAN encap (VNI of VPC-2) → syfrah0 → Node 2
Node 2: → VXLAN decap → bridge-2 → tap-B → VM-B in VPC-2
```

## Supported topologies

### Simple (default VPC)
```
Project A → default VPC (VNI 100, 10.1.0.0/16)
  └── production env
        ├── subnet frontend (10.1.1.0/24) → web-1, web-2
        └── subnet database (10.1.2.0/24) → db-1
```

### Isolated VPC
```
Project A → default VPC (VNI 100, 10.1.0.0/16) → staging
Project A → prod VPC (VNI 101, 10.2.0.0/16)    → production
```
Different VNIs = completely separate L2 domains. Zero connectivity.

### Shared VPC
```
Org → platform VPC (VNI 200, 10.100.0.0/16) [shared]
  ├── attached: Project A → subnet monitoring (10.100.1.0/24)
  └── attached: Project B → subnet monitoring (10.100.2.0/24)
```
Same VNI, same bridge. VMs from different projects share the network.

### Hub & Spoke
```
hub VPC (VNI 300)   ←peer→  spoke-a VPC (VNI 301)
hub VPC (VNI 300)   ←peer→  spoke-b VPC (VNI 302)
spoke-a ✗ spoke-b   (no peering = no route)
```

## CLI UX — full workflow

```bash
# 1. Setup org structure
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme

# 2. Create subnets (default VPC auto-created with VNI)
syfrah subnet create frontend --env production --project backend --org acme
syfrah subnet create database --env production --project backend --org acme

# 3. Create VMs
syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048 --ssh-key ~/.ssh/id.pub

# Output:
#   VM created: web-1
#     IP:       10.1.1.3
#     Subnet:   frontend (10.1.1.0/24)
#     VPC:      default (VNI 100, 10.1.0.0/16)
#     Node:     node-1
#     Phase:    Running

# 4. SSH in
ssh root@10.1.1.3

# 5. List VMs
syfrah compute vm list --project backend --org acme
# NAME   IMAGE        PHASE    IP          SUBNET     VPC      NODE    vCPUs  MEMORY
# web-1  alpine-3.20  Running  10.1.1.3    frontend   default  node-1  2      2048
# db-1   alpine-3.20  Running  10.1.2.3    database   default  node-1  4      8192

# 6. Multi-node: add a second server
syfrah fabric join 203.0.113.1 --pin 4829
syfrah compute vm create --name web-2 --image alpine-3.20 --subnet frontend \
  --project backend --org acme --vcpus 2 --memory 2048 --ssh-key ~/.ssh/id.pub

# web-1 (node-1) and web-2 (node-2) ping each other — VXLAN over WireGuard
ssh root@10.1.1.3 "ping 10.1.1.4"

# 7. Isolated production
syfrah vpc create prod-isolated --project backend --org acme --cidr 10.2.0.0/16
syfrah env create prod-secure --project backend --org acme
syfrah subnet create api --env prod-secure --project backend --org acme --vpc prod-isolated

# 8. Shared monitoring
syfrah vpc create monitoring --org acme --shared --cidr 10.100.0.0/16
syfrah vpc attach monitoring --project backend
syfrah vpc attach monitoring --project user-service

# 9. Hub & spoke
syfrah vpc peer --from hub-vpc --to spoke-a-vpc
syfrah vpc peer --from hub-vpc --to spoke-b-vpc
```

## Crate dependency graph

```
syfrah-core (types, crypto, addressing)
    ↑
syfrah-org (Org, Project, Env, VPC, Subnet, Peering, IPAM)
    ↑
syfrah-overlay (VXLAN, bridge, TAP, FDB, nftables, NAT, ARP proxy)
    ↑
syfrah-compute (VM lifecycle — calls overlay for networking)

syfrah-fabric (WireGuard mesh — provides transport for VXLAN)
    ↑
syfrah-overlay (uses fabric peer list for VTEP discovery + VM placement announcements)
```

## System requirements

- `iproute2` (bridge, TAP, veth, VXLAN, routes, FDB, neighbor)
- `nftables` (firewall, NAT, anti-spoofing, isolation)
- `genisoimage` (config-drive ISO)
- Root / `NET_ADMIN`
- Kernel with VXLAN support (standard on all modern Linux, `modprobe vxlan`)

## Testing strategy

- **Unit tests**: mock `NetworkBackend` — verify call sequences, FDB entries, nftables rule generation
- **Integration tests**: Linux network namespaces in Docker CI — real bridges, TAPs, VXLAN (kernel module available in CI runners), nftables
- **E2E single-node**: on test server — `vm create` → SSH → cross-subnet ping → `vm delete`
- **E2E multi-node**: two test servers — `vm create` on each → cross-node ping over VXLAN/WireGuard

## Rejected alternatives

1. **Bridge-only without VXLAN**: rejected — VXLAN is the product. Without it, Syfrah is just a local VM manager. VXLAN over WireGuard is what enables multi-AZ private networking across providers.

2. **Hardcoded subnet, no Org model**: rejected — creates tech debt, doesn't support isolation topologies.

3. **1 env = 1 subnet**: rejected — too limiting. Production needs multiple subnets (frontend, database, internal) with different security postures.

4. **No security groups from day 1**: rejected — networking without isolation is a security regression. Anti-spoofing + default-deny is mandatory.

5. **DHCP instead of config-drive**: rejected — config-drive is simpler, no daemon, no network-based attack surface.

6. **Flood-and-learn FDB**: rejected — the control plane knows where every VM is. Static FDB = zero broadcast, deterministic forwarding, no convergence delay.

7. **MTU 1500 initially, adjust later**: rejected — changing MTU on running VMs breaks TCP connections. Set 1350 from day 1 so nothing breaks when multi-node activates.

8. **IPAM without allocation tracking**: rejected — the bitmap alone doesn't capture the allocation lifecycle. A crash between IP allocation and VM boot creates orphaned IPs that the bitmap can't explain. The `ip_allocations` table is the audit trail.

## Commercial value

This architecture delivers:

- **Multi-AZ private networking**: VMs on servers in different datacenters communicate over encrypted private IPs — like a unified cloud across providers
- **Provider-agnostic**: mix OVH, Hetzner, Scaleway servers in the same VPC
- **Zero-trust networking**: WireGuard encryption on every inter-node packet, nftables isolation per VM, anti-spoofing enforced at the host
- **VPC isolation via VXLAN VNI**: each VPC is a separate L2 domain. Isolation is enforced at the overlay level, not just firewall rules. Comparable in model to major cloud VPCs, though without the scale guarantees of hyperscaler infrastructure.
- **No central network appliance**: fully distributed data plane, no single point of failure for networking
- **Deterministic forwarding**: static FDB + ARP proxy = no broadcast storms, no convergence delay, no MAC learning races
- **Instant VM networking**: VM gets its IP at boot via config-drive, FDB is pre-populated before first packet, zero warm-up

## Future additions (out of scope for this ADR)

- **Private DNS**: CoreDNS per node, `{vm}.{vpc}.syfrah.internal`
- **Floating IPs**: DNAT for IPv4 ingress
- **IPv6 public**: direct routing, no NAT
- **Security group CRUD**: user-configurable rules via CLI
- **Custom DNS records**: tenant-managed DNS
- **Budgets and labels**: cost tracking per environment
- **TTL enforcement**: auto-destroy expired environments
- **Multi-NIC VMs**: attach a VM to multiple subnets/VPCs

## References

- `layers/overlay/README.md` — full overlay design
- `layers/org/README.md` — organization model
- `handbook/ARCHITECTURE.md` — global architecture
- Issue #711 — architecture debate
- Issue #669 — compute audit
