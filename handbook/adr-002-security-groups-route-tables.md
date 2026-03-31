# ADR-002: Security Groups + Route Tables + Network Resources

**Status**: Accepted
**Date**: 2026-03-30
**Decided by**: Orchestrator + Sacha, after team debate on #861

## Context

ADR-001 delivered the networking foundation: VPCs with VXLAN isolation, subnets, IPAM, bridges, TAPs, FDB distribution, config-drive network injection, and compute integration. VMs can communicate within a VPC, but all traffic policy is hardcoded — default-deny ingress (SSH + ICMP only), default-allow egress, SNAT masquerade for internet.

The operator has zero runtime control over network policy. There are no security groups, no route tables, no NAT gateways as manageable resources. This is a critical gap: any production cloud requires per-VM firewall rules, explicit routing decisions, and operator-managed internet egress.

This ADR introduces the complete network policy layer:

- **Security Groups** — per-NIC stateful firewalls with allow-only rules, enforced via nftables
- **Route Tables** — per-VPC routing with system/user/propagated routes, subnet association
- **NAT Gateway** — explicit resource for internet egress (no implicit SNAT)
- **Network Interface (NIC)** — first-class object that SGs attach to
- **ResourceState** — unified lifecycle for all network objects

Design principles:

1. **Every network resource is explicit.** No implicit internet egress, no hidden routes, no magic defaults that bypass operator intent.
2. **SGs attach to NICs, not VMs.** This models reality (a multi-NIC VM needs different policies per NIC) and aligns with AWS/GCP/Azure.
3. **Routes point to resource IDs, not abstract strings.** A route targeting a NAT Gateway references `NatGateway(NatGatewayId)`, and the route is only active if the target is in `Active` state.
4. **Deletion guards on everything.** No resource can be deleted if other resources depend on it.

## Network Resource Model

This section defines the foundational types that all phases build on.

### ResourceState

Every mutable network resource (NIC, NAT Gateway, VPC Peering, Route Table, Security Group) carries a `ResourceState`:

```
enum ResourceState {
    Pending,      // creation in progress
    Active,       // operational
    Failed,       // creation/operation failed
    Deleting,     // teardown in progress
    Deleted,      // terminal — soft-deleted, retained for audit
}
```

State transitions:

```
Pending → Active      (creation succeeded)
Pending → Failed      (creation failed)
Active  → Deleting    (deletion initiated)
Active  → Failed      (runtime failure detected)
Deleting → Deleted    (teardown complete)
Failed  → Deleting    (operator forces deletion)
Failed  → Active      (retry succeeded — reconciliation)
```

Rules:
- A resource in `Pending` or `Failed` state cannot be used as a route target, SG reference, or NIC attachment — the system treats it as unavailable.
- A resource in `Deleting` or `Deleted` state rejects all mutations.
- The reconciliation loop detects `Pending` resources older than 5 minutes and transitions them to `Failed`.
- `Deleted` resources are retained for a configurable retention period (default 24h) then hard-deleted.

### NetworkInterface (NIC)

The NIC is the attachment point for security groups. Every VM has at least one NIC.

```
NetworkInterface {
    id: NicId,
    name: String,
    vm_id: Option<VmId>,       // None if detached
    subnet_id: SubnetId,
    vpc_id: VpcId,
    private_ip: String,        // from IPAM
    mac: String,               // derived from IP (02:00:{ip_hex})
    security_groups: Vec<SecurityGroupId>,
    state: ResourceState,
    created_at: u64,
}
```

Lifecycle:
- **Created automatically** during `vm create`. The NIC is created as `Pending`, IPAM allocates the IP, the TAP/veth is wired, then the NIC transitions to `Active`.
- **Phase 1**: one NIC per VM. The NIC is always the VM's primary NIC.
- **Future**: multi-NIC support. A VM can have NICs in different subnets/VPCs. Each NIC gets its own SG set.
- **Destroyed** on `vm delete`. NIC transitions to `Deleting`, nftables rules are flushed, TAP is deleted, IP is released, NIC transitions to `Deleted`.

SG attachment:
- Security groups are attached to the NIC, not the VM.
- UX shortcut: `syfrah sg attach <sg> --vm <vm>` resolves to the VM's primary NIC. The full form is `syfrah sg attach <sg> --nic <nic>`.
- The NIC's `security_groups` vector is ordered. When generating nftables rules, rules from all attached SGs are merged and ordered by priority.

Persistence: redb table `network_interfaces` (nic_id → NIC record).

### NAT Gateway

NAT Gateway is an explicit resource that provides internet egress via SNAT. There is no implicit internet access — the operator must create a NAT Gateway and add a route pointing to it.

```
NatGateway {
    id: NatGatewayId,
    name: String,
    vpc_id: VpcId,
    subnet_id: SubnetId,       // NAT GW lives in a specific subnet
    public_ip: String,         // the node's public IP used for SNAT
    state: ResourceState,
    created_at: u64,
}
```

Design:
- A NAT Gateway is placed in a subnet and uses the hosting node's public IP for SNAT masquerade.
- Route tables reference `NatGateway(NatGatewayId)` as a target for `0.0.0.0/0` routes.
- If the NAT Gateway is not in `Active` state, routes targeting it show status `blackhole (target unavailable)` and traffic is dropped.
- Phase 1 (single-node): one NAT GW maps to one nftables masquerade chain. Multi-node: each node hosting VMs that route through the NAT GW applies the masquerade locally using the NAT GW's configured public IP.

Lifecycle:
1. `syfrah nat-gw create <name> --vpc <vpc> --subnet <subnet>` → state `Pending`
2. System configures nftables masquerade chain → state `Active`
3. `syfrah nat-gw delete <name>` → state `Deleting` (if no routes target it) → `Deleted`

Persistence: redb table `nat_gateways` (nat_gw_id → NatGateway record).

### Internet Gateway (future)

Not in scope for this ADR. When implemented, it will be a resource similar to NAT Gateway but for inbound traffic (DNAT / floating IPs). Route tables will support `InternetGateway(IgwId)` as a target.

## Phase 1 — Security Groups

### Model

```
SecurityGroup {
    id: SecurityGroupId,
    name: String,
    description: String,
    vpc_id: VpcId,
    state: ResourceState,
    created_at: u64,
    updated_at: u64,
}

SecurityGroupRule {
    id: RuleId,
    sg_id: SecurityGroupId,
    direction: Direction,
    protocol: Protocol,
    port_range: Option<PortRange>,
    source: TrafficSource,       // for ingress: where traffic comes from
    destination: TrafficSource,  // for egress: where traffic goes to
    priority: u32,               // lower = evaluated first
    description: String,
    created_at: u64,
}

enum Direction {
    Ingress,
    Egress,
}

enum Protocol {
    Tcp,
    Udp,
    Icmp,
    All,
}

struct PortRange {
    from: u16,
    to: u16,   // inclusive. Single port: from == to
}

enum TrafficSource {
    Cidr(Ipv4Net),
    SecurityGroup(SecurityGroupId),
}
```

### Default Security Group

Every VPC gets a default security group auto-created when the VPC is created. The default SG:

```
Name: default
Description: Default security group for VPC {vpc_name}

DIRECTION  PROTOCOL  PORTS   SOURCE/DEST    PRIORITY  DESCRIPTION
ingress    tcp       22-22   0.0.0.0/0      100       SSH access
ingress    icmp      -       0.0.0.0/0      200       ICMP (ping)
egress     all       -       0.0.0.0/0      100       All outbound
```

Rules:
- The default SG **cannot be deleted** (deletion guard).
- Rules on the default SG **can be modified** by the operator.
- Every NIC that does not have an explicit SG attached gets the default SG. When `vm create` runs without `--sg`, the VM's NIC is attached to the VPC's default SG.
- The default SG matches the current hardcoded nftables rules, so the migration (Phase 1, issue 15) replaces hardcoded rules with the default SG without behavioral change.

### SG-to-SG References

A rule's source (ingress) or destination (egress) can be another security group:

```bash
syfrah sg add-rule db-sg --direction ingress --protocol tcp --port 5432 \
  --source sg:web-sg --priority 100
```

This means: "allow TCP 5432 inbound from any NIC that has `web-sg` attached."

Implementation:
- When generating nftables rules, SG-to-SG references are resolved to nftables **named sets** containing the IPs of all NICs attached to the referenced SG.
- When a NIC is attached/detached from a referenced SG, all SGs that reference it must regenerate their nftables rules (the named set is updated).
- Circular references are allowed (SG-A references SG-B and vice versa) — they resolve to IP sets, not recursive rule expansion.

Deletion guard: a security group cannot be deleted if it is referenced by rules in other security groups.

### nftables Architecture

Per-VM chain architecture with vmap dispatch. All SG rules live in the `filter` table under the `bridge` family (for bridge-level filtering) and the `inet` family (for L3 filtering).

```
table bridge syfrah_sg {
    # Dispatch chain — maps TAP interface to per-VM chain via vmap
    chain dispatch_ingress {
        type filter hook forward priority 0; policy drop;
        ct state established,related accept
        ibrdev vmap {
            "syftap-{hash_a}": goto vm_{hash_a}_in,
            "syftap-{hash_b}": goto vm_{hash_b}_in,
        }
    }

    chain dispatch_egress {
        type filter hook forward priority 0; policy drop;
        ct state established,related accept
        obrdev vmap {
            "syftap-{hash_a}": goto vm_{hash_a}_out,
            "syftap-{hash_b}": goto vm_{hash_b}_out,
        }
    }

    # Per-VM ingress chain (traffic entering the VM's TAP)
    chain vm_{hash_a}_in {
        # Anti-spoofing (destination MAC/IP must match IPAM)
        # Rules from all SGs attached to this NIC, ordered by priority
        tcp dport 22 accept                  # SG rule: SSH
        icmp type echo-request accept        # SG rule: ICMP
        drop                                 # default deny
    }

    # Per-VM egress chain (traffic leaving the VM's TAP)
    chain vm_{hash_a}_out {
        # Anti-spoofing (source MAC/IP must match IPAM)
        # Egress rules, ordered by priority
        accept                               # default: all egress allowed
    }

    # Named sets for SG-to-SG references
    set sg_{web_sg_hash}_ips {
        type ipv4_addr
        elements = { 10.1.1.3, 10.1.1.4 }
    }
}
```

Key design decisions:
- **vmap dispatch**: O(1) lookup to find the correct per-VM chain. No linear chain of `iif` matches.
- **Per-VM chains**: each VM's rules are isolated. Updating one VM's SGs does not touch other VMs' chains.
- **Anti-spoofing stays**: source MAC/IP validation is always the first rule in every egress chain. Destination validation (where applicable) in ingress chains. This is not part of SG rules — it's a system-enforced invariant.
- **conntrack first**: `ct state established,related accept` at the top of dispatch chains. Stateful — return traffic is always allowed.
- **Named sets for SG references**: when a rule references another SG, the source/destination is an nftables named set. The set contains the IPs of all NICs attached to that SG. Sets are updated atomically.

### Rule Application Lifecycle

When SG rules change (add/remove rule, attach/detach SG to NIC, NIC created/deleted):

1. **Collect**: gather all SGs attached to the affected NIC(s).
2. **Merge**: merge all rules from all attached SGs.
3. **Sort**: order rules by priority (ascending — lower priority number = evaluated first).
4. **Generate**: produce nftables chain rules.
5. **Apply**: atomic swap of the per-VM chain (create new chain, swap in dispatch vmap, delete old chain).

This ensures zero-downtime rule updates. At no point is a VM left without firewall rules.

### Egress Model

The egress model follows the AWS pattern and is **locked for Phase 1**:

- **No egress rules on the SG** → **all egress is allowed**. This is the default. The per-VM egress chain ends with `accept`.
- **If ANY egress rule exists on any attached SG** → **only matching egress traffic is allowed, all other egress is denied**. The per-VM egress chain ends with `drop` instead of `accept`.

This means: adding the first egress rule to an SG **changes behavior** for all NICs using that SG. The CLI must warn:

```
Warning: Adding an egress rule will restrict all egress traffic for VMs
using this security group. Only traffic matching egress rules will be
allowed. Currently, all egress is permitted.
Proceed? [y/N]
```

The default SG ships with an explicit `egress all 0.0.0.0/0 allow` rule, so the default behavior is unchanged (all egress allowed). But if the operator removes that rule and adds specific egress rules, the behavior changes to restrictive egress.

### Priority

Every rule has a `priority: u32` field. Lower value = evaluated first in the nftables chain.

Justification:
1. **Deterministic evaluation order**: rules are inserted into nftables chains in priority order. Two rules matching the same traffic are evaluated in priority order — the first match wins (accept).
2. **Deterministic rendering**: `syfrah sg rules <sg>` displays rules sorted by priority. Operators see a consistent, predictable list.
3. **Future deny extension**: when deny rules are added (Phase N), priority determines whether an allow or deny is evaluated first. The priority field is already in place.

In Phase 1 (allow-only model): priority affects only rule ordering in nft chains and display. Since all rules are `accept`, the security semantics are the same regardless of order — but the chain evaluation short-circuits on first match, so higher-priority (lower number) rules that match more traffic improve performance.

Default priority conventions:
- 100: standard rules
- 50: high-priority rules
- 200: low-priority / catch-all rules

If no priority is specified, default is 100.

### CLI

```bash
# Security group CRUD
syfrah sg create <name> --vpc <vpc> [--description "..."]
syfrah sg list [--vpc <vpc>]
syfrah sg show <name> [--vpc <vpc>]
syfrah sg delete <name> [--vpc <vpc>]

# Rule management
syfrah sg add-rule <sg> \
  --direction ingress|egress \
  --protocol tcp|udp|icmp|all \
  [--port <port>|--port-range <from>-<to>] \
  --source <cidr>|sg:<sg-name> \
  [--priority <n>] \
  [--description "..."]

syfrah sg remove-rule <sg> --rule-id <id>
syfrah sg rules <sg>

# Attach/detach (VM shortcut resolves to primary NIC)
syfrah sg attach <sg> --vm <vm>
syfrah sg attach <sg> --nic <nic>
syfrah sg detach <sg> --vm <vm>
syfrah sg detach <sg> --nic <nic>

# Diagnostic
syfrah sg check [--vm <vm>] [--nic <nic>]
```

### Persistence

redb tables:
- `security_groups` (sg_id → SecurityGroup)
- `sg_rules` (rule_id → SecurityGroupRule, secondary index: sg_id)
- `sg_attachments` (nic_id → Vec<SecurityGroupId>, secondary index: sg_id → Vec<NicId>)
- `network_interfaces` (nic_id → NetworkInterface)

All mutations go through the daemon's control socket. The CLI sends requests; the daemon owns the DB exclusively.

### Migration

The current hardcoded nftables rules in `apply_vm_rules()` must be replaced with the default SG:

1. On daemon startup, check if VPCs exist without a default SG. If so, create the default SG with the standard rules (SSH, ICMP ingress; all egress).
2. For each existing VM/NIC without an SG attachment, attach the default SG.
3. Regenerate nftables rules from the SG model.
4. Remove the hardcoded `apply_vm_rules()` path — all firewall rules now come from SGs.

This migration is backward-compatible: the default SG produces the same nftables rules as the current hardcoded path.

### Deletion Guards

**SecurityGroup**:
- Cannot delete if any NIC has this SG attached (error: "security group is attached to N network interfaces").
- Cannot delete if any rule in another SG references this SG as a source/destination (error: "security group is referenced by rules in: sg-A, sg-B").
- Cannot delete the default SG (error: "cannot delete the default security group").

**SecurityGroupRule**:
- Can always be deleted (no reverse dependencies). Deletion triggers nftables regeneration for all NICs using the parent SG.

**NetworkInterface**:
- Cannot delete if the VM is in `Running` or `Stopping` state (error: "cannot delete NIC while VM is running — stop the VM first").
- NIC deletion is normally triggered by `vm delete`, which stops the VM first.

## Phase 2 — Route Tables

### Model

```
RouteTable {
    id: RouteTableId,
    name: String,
    vpc_id: VpcId,
    is_default: bool,
    state: ResourceState,
    created_at: u64,
}

Route {
    id: RouteId,
    route_table_id: RouteTableId,
    destination: Ipv4Net,          // e.g. 10.1.1.0/24, 0.0.0.0/0
    target: RouteTarget,
    origin: RouteOrigin,
    status: RouteStatus,
    priority: u32,
    created_at: u64,
}

enum RouteTarget {
    Local,                          // system-managed, delivers to local subnet
    NatGateway(NatGatewayId),       // internet egress via explicit NAT GW
    VpcPeering(PeeringId),          // cross-VPC via explicit peering
    Blackhole,                       // explicit drop
}

enum RouteOrigin {
    System,      // auto-created, not deletable (local subnet routes, VPC CIDR)
    User,        // operator-created, fully manageable
    Propagated,  // auto-created from peering, auto-managed
}

enum RouteStatus {
    Active,                       // target resource exists and is Active
    Blackhole,                    // target resource is unavailable (Failed/Deleting/Deleted)
}
```

### System vs User Routes

**System routes** (origin: `System`):
- Created automatically when a subnet is created: `{subnet_cidr} → Local`
- Created automatically for the VPC CIDR: `{vpc_cidr} → Local`
- Cannot be deleted or modified by the operator.
- Always have the highest priority (lowest number, e.g., 0).

**User routes** (origin: `User`):
- Created explicitly by the operator via `syfrah route add`.
- Can be modified and deleted.
- Default priority: 100.

**Propagated routes** (origin: `Propagated`):
- Auto-created when a VPC peering is established.
- For each peered VPC, a route is added: `{peer_vpc_cidr} → VpcPeering(peering_id)`.
- Auto-removed when the peering is deleted.
- Cannot be manually deleted (managed by the peering lifecycle).
- Priority: 50 (higher than user routes by default).

Route evaluation: most-specific prefix wins (longest prefix match). If two routes have the same prefix, lower priority number wins. If same prefix and same priority, system > propagated > user.

### Default Route Table

Every VPC gets a default route table auto-created when the VPC is created.

Initial contents (for a VPC with CIDR 10.1.0.0/16 and one subnet 10.1.1.0/24):

```
DESTINATION    TARGET                    ORIGIN      STATUS    PRIORITY
10.1.0.0/16    Local                     system      active    0
10.1.1.0/24    Local                     system      active    0
```

Note: there is **no** `0.0.0.0/0 → NAT` route by default. Internet egress requires the operator to:
1. Create a NAT Gateway: `syfrah nat-gw create my-nat --vpc default --subnet frontend`
2. Add a route: `syfrah route add --vpc default --destination 0.0.0.0/0 --target nat-gw:my-nat`

This is a deliberate departure from the v1 behavior (implicit SNAT). Explicit is better than implicit.

### Subnet Association

Each subnet is associated with exactly one route table:
- By default, a subnet uses its VPC's default route table.
- The operator can associate a subnet with a custom route table: `syfrah route table associate <table> --subnet <subnet>`.
- A subnet can only be associated with one route table at a time. Re-associating replaces the previous association.
- A route table can be associated with multiple subnets.

When a packet leaves a VM, the system looks up the VM's subnet → route table → routes → longest prefix match → target.

### Route Target Validation

When a route is created, the target resource must exist and be in `Active` state:
- `NatGateway(id)`: the NAT GW must exist and be `Active`.
- `VpcPeering(id)`: the peering must exist and be `Active`.
- `Blackhole`: always valid (no resource reference).
- `Local`: always valid (system-managed).

When a target resource transitions out of `Active` state (e.g., NAT GW goes to `Failed`):
- The route's status changes to `Blackhole`.
- Traffic matching this route is dropped.
- `syfrah route list` shows: `0.0.0.0/0  nat-gw:my-nat  user  blackhole (target unavailable)  100`.
- When the resource returns to `Active`, the route status automatically returns to `Active`.

This validation runs:
1. At route creation time (reject if target is not Active).
2. Continuously via the reconciliation loop (update status if target state changes).

### CLI

```bash
# Route table CRUD
syfrah route table create <name> --vpc <vpc>
syfrah route table list [--vpc <vpc>]
syfrah route table show <name> [--vpc <vpc>]
syfrah route table delete <name> [--vpc <vpc>]
syfrah route table associate <table> --subnet <subnet>
syfrah route table disassociate --subnet <subnet>

# Route management
syfrah route add --table <table> --destination <cidr> --target <target> [--priority <n>]
syfrah route delete --table <table> --destination <cidr>
syfrah route list --table <table>
syfrah route list --vpc <vpc>     # show all routes across all tables in the VPC
```

Target syntax in CLI:
- `local` — Local
- `nat-gw:<name>` — NatGateway
- `peering:<name>` — VpcPeering
- `blackhole` — Blackhole

### Deletion Guards

**RouteTable**:
- Cannot delete if any subnet is associated with it (error: "route table is associated with N subnets — disassociate them first").
- Cannot delete the default route table (error: "cannot delete the default route table").

**Route**:
- Cannot delete a system route (error: "cannot delete system-managed route").
- Cannot delete a propagated route (error: "cannot delete propagated route — remove the peering instead").
- User routes can always be deleted.

## Phase 3 — NAT Gateway

### Model

See the NAT Gateway definition in the Network Resource Model section above.

Additional detail:

```
NatGateway {
    id: NatGatewayId,
    name: String,
    vpc_id: VpcId,
    subnet_id: SubnetId,
    public_ip: String,
    state: ResourceState,
    created_at: u64,
}
```

The NAT Gateway is the **only** path for internet egress. Without a NAT GW + route, VMs have no internet access. This is intentional: an operator who does not create a NAT GW has a fully isolated VPC.

### CLI

```bash
syfrah nat-gw create <name> --vpc <vpc> --subnet <subnet>
syfrah nat-gw list [--vpc <vpc>]
syfrah nat-gw show <name> [--vpc <vpc>]
syfrah nat-gw delete <name> [--vpc <vpc>]
```

On `nat-gw create`:
1. Validate VPC and subnet exist and are Active.
2. Determine the node's public IP (the host's outbound IP for the subnet's node).
3. Create the NAT GW record in `Pending` state.
4. Configure nftables masquerade: packets from VMs in this VPC, routed through this NAT GW, are SNATed to `public_ip`.
5. Transition to `Active`.

On `nat-gw delete`:
1. Check deletion guards (no routes targeting this NAT GW).
2. Transition to `Deleting`.
3. Remove nftables masquerade chain.
4. Transition to `Deleted`.

### Integration with Route Tables

The NAT GW becomes usable only when a route points to it:

```bash
syfrah nat-gw create egress-nat --vpc default --subnet frontend
syfrah route add --table default --destination 0.0.0.0/0 --target nat-gw:egress-nat
```

Now VMs in subnets using the default route table can reach the internet via SNAT through the NAT GW's public IP.

If the NAT GW fails or is deleted:
- Routes targeting it transition to `Blackhole` status.
- Traffic is dropped (no silent fallback to another path).
- Operator must fix the NAT GW or create a new one and update routes.

### Deletion Guards

**NatGateway**:
- Cannot delete if any route targets this NAT GW (error: "NAT gateway is targeted by N routes — delete the routes first").

## VPC Peering Lifecycle

The peering model from ADR-001 is extended with a proper lifecycle:

```
VpcPeering {
    id: PeeringId,
    vpc_a: VpcId,
    vpc_b: VpcId,
    state: PeeringState,
    created_at: u64,
}

enum PeeringState {
    PendingAcceptance,    // initiator created, waiting for acceptor
    Active,               // both sides accepted, routes propagated
    Rejected,             // acceptor rejected
    Failed,               // setup failed
    Deleting,             // teardown in progress
}
```

Phase 1 (single-org, trust model): peering is auto-accepted. `vpc peer --from A --to B` immediately transitions to `Active`, and propagated routes are added to both VPCs' default route tables.

Future (multi-org): the acceptor org must explicitly accept. Peering stays in `PendingAcceptance` until accepted. No routes are propagated until `Active`.

On peering creation (`Active`):
1. Add propagated route to VPC-A's default route table: `{vpc_b_cidr} → VpcPeering(peering_id)`.
2. Add propagated route to VPC-B's default route table: `{vpc_a_cidr} → VpcPeering(peering_id)`.
3. Configure veth pair (same node) or cross-VNI forwarding rules (multi-node).

On peering deletion:
1. Transition to `Deleting`.
2. Remove propagated routes from both VPCs.
3. Remove veth pair / forwarding rules.
4. Transition to deleted (record removed).

## Interaction: SG x Routes x NAT GW

### Packet Flow (complete)

**VM-to-VM, same subnet:**
```
VM-A (10.1.1.3) sends packet to 10.1.1.5
  1. Egress SG check on VM-A's NIC
     → egress chain: anti-spoofing (src MAC/IP) → egress rules → accept/drop
  2. Bridge forwarding (local FDB lookup)
  3. Ingress SG check on VM-B's NIC
     → ingress chain: conntrack (established?) → ingress rules → accept/drop
  4. Delivered to VM-B
```

**VM-to-VM, different subnets (same VPC):**
```
VM-A (10.1.1.3) sends packet to 10.1.2.3
  1. Egress SG check on VM-A's NIC
  2. Route lookup: VM-A's subnet → route table → 10.1.2.0/24 → Local
  3. Bridge L3 routing via gateway IPs
  4. Ingress SG check on VM-B's NIC
  5. Delivered to VM-B
```

**VM-to-internet (via NAT GW):**
```
VM-A (10.1.1.3) sends packet to 8.8.8.8
  1. Egress SG check on VM-A's NIC
  2. Route lookup: VM-A's subnet → route table → 0.0.0.0/0 → NatGateway(nat-gw-id)
     → Check: NAT GW state == Active? Yes → proceed. No → drop (blackhole).
  3. nftables masquerade: SNAT src 10.1.1.3 → NAT GW's public IP
  4. Packet exits node via public interface
  5. Return traffic: conntrack match → reverse SNAT → route to VM-A → ingress SG (established) → VM-A
```

**VM-to-VM, peered VPCs:**
```
VM-A in VPC-1 (10.1.1.3) sends to VM-B in VPC-2 (10.2.1.3)
  1. Egress SG check on VM-A's NIC
  2. Route lookup: 10.2.0.0/16 → VpcPeering(peering-id)
     → Check: peering state == Active? Yes → proceed. No → drop.
  3. Bridge forwarding via veth peer (same node) or VXLAN (cross-node)
  4. Ingress SG check on VM-B's NIC
  5. Delivered to VM-B
```

**VM-to-blackhole:**
```
VM-A (10.1.1.3) sends packet to 10.3.0.0/16 (blackhole route)
  1. Egress SG check on VM-A's NIC
  2. Route lookup: 10.3.0.0/16 → Blackhole
  3. Packet dropped. Counter incremented.
```

### Evaluation Order

For every packet leaving or entering a VM:

1. **Anti-spoofing** (always first, system-enforced, not byppassable)
2. **Conntrack** (established/related → accept immediately)
3. **Security Group rules** (ordered by priority within merged rule set)
4. **Route table lookup** (longest prefix match on destination)
5. **Route target validation** (is the target resource Active?)
6. **Forwarding** (bridge, veth peer, VXLAN, or masquerade)

## nftables Implementation Detail

The complete nftables structure after all phases:

```
# Bridge-level filtering (SG enforcement)
table bridge syfrah_sg {
    chain dispatch_in {
        type filter hook forward priority 0; policy drop;
        ct state established,related accept
        ibrdev vmap { ... per-VM chains ... }
    }

    chain dispatch_out {
        type filter hook forward priority 0; policy drop;
        ct state established,related accept
        obrdev vmap { ... per-VM chains ... }
    }

    # Per-VM chains (one pair per VM)
    chain vm_{hash}_in { ... }
    chain vm_{hash}_out { ... }

    # Named sets for SG-to-SG references
    set sg_{hash}_ips { type ipv4_addr; elements = { ... }; }
}

# IP-level filtering (anti-spoofing)
table inet syfrah_spoof {
    chain antispoof {
        type filter hook forward priority -10; policy accept;
        # Per-TAP anti-spoofing rules
        iifname "syftap-{hash}" ether saddr != {expected_mac} drop
        iifname "syftap-{hash}" ip saddr != {expected_ip} drop
    }
}

# NAT (masquerade for NAT GW)
table ip syfrah_nat {
    chain postrouting {
        type nat hook postrouting priority 100; policy accept;
        # Per-NAT-GW masquerade rules
        ip saddr {vpc_cidr} oifname {public_iface} snat to {nat_gw_public_ip}
    }
}

# Route enforcement (blackhole routes)
table inet syfrah_routes {
    chain prerouting {
        type filter hook prerouting priority -5; policy accept;
        # Blackhole routes — explicit drops
        ip daddr {blackhole_cidr} drop
    }
}
```

Chain swap for atomic updates:
1. Create new chain `vm_{hash}_in_new` with updated rules.
2. Update vmap entry to point to new chain.
3. Flush and delete old chain.

This ensures no packet is processed without a complete rule set.

## Reconciliation + State Management

The existing reconciliation loop (ADR-001, 30-second interval) is extended:

### SG Reconciliation

1. **Desired state**: for each NIC in `Active` state, collect attached SGs → merge rules → generate expected nftables chains.
2. **Actual state**: read current nftables chains via `nft list table bridge syfrah_sg`.
3. **Diff**: compare expected vs actual.
4. **Apply**: if diverged, regenerate and apply atomically.

Triggers for immediate (event-driven) reconciliation:
- SG rule added/removed
- SG attached/detached from NIC
- VM created/deleted
- NIC in referenced SG added/removed (named set update)

### Route Reconciliation

1. **Desired state**: for each route table, collect routes → check target resource states → generate expected nftables/ip-route rules.
2. **Actual state**: read current routes via `ip route` and nftables blackhole rules.
3. **Diff**: compare and apply.
4. **Target state check**: for each route with a resource target (NAT GW, peering), verify the target resource's `ResourceState`. Update route status accordingly.

### NAT GW Reconciliation

1. Verify masquerade rules exist for all `Active` NAT GWs.
2. Remove masquerade rules for NAT GWs not in `Active` state.
3. Check NAT GW health (can the public IP reach the internet?). If not, transition to `Failed`.

### ResourceState Drift

For all resources with `ResourceState`:
- `Pending` for > 5 minutes → transition to `Failed`.
- `Deleting` for > 2 minutes → force cleanup and transition to `Deleted`.
- `Failed` resources → attempt recovery (re-apply nftables, re-create masquerade). If recovery succeeds → `Active`.

## Error Handling

### Creation Failures

If any step in resource creation fails, the resource transitions to `Failed` state with a reason:

```
NatGateway { state: Failed, ... }
→ syfrah nat-gw show my-nat
  State: Failed (nftables masquerade setup failed: permission denied)
```

The operator can retry (`nat-gw delete` then `nat-gw create`) or wait for reconciliation to retry automatically.

### Rule Application Failures

If nftables rule application fails:
1. Log the error with full context (which VM, which SG, which rules).
2. The per-VM chain retains its previous rules (atomic swap means the old chain is still active).
3. The reconciliation loop retries on the next cycle.
4. `syfrah sg check` reports the discrepancy.

### Dependency Failures

If a referenced resource is unavailable:
- SG references a deleted SG → rule is marked invalid, skip during nftables generation, warn in `sg check`.
- Route targets a Failed NAT GW → route status = Blackhole, traffic dropped, `route list` shows the issue.

## Monitoring

### sg check

`syfrah sg check` is an SG-only diagnostic command. It verifies:

1. **Rule consistency**: all SG rules can be translated to valid nftables rules.
2. **Reference resolution**: all SG-to-SG references point to existing, Active SGs.
3. **nftables sync**: the actual nftables chains match the expected state from SG definitions.
4. **Attachment validity**: all NIC attachments reference existing NICs and SGs.

Output:

```
$ syfrah sg check
Checking security groups...

  sg: default (vpc: default)
    ✓ 3 rules valid
    ✓ attached to 4 NICs
    ✓ nftables chains in sync

  sg: web-sg (vpc: default)
    ✓ 3 rules valid
    ✗ rule #5 references sg:deleted-sg — target not found
    ✓ attached to 2 NICs
    ✗ nftables chain for vm-abc123 out of sync — will reconcile

  2 issues found. Run with --fix to reconcile.
```

`syfrah sg check --fix`: triggers immediate reconciliation for all detected issues.

### Future: syfrah network check

A comprehensive network diagnostic (not in this ADR's scope) that covers:
- SG rules (everything `sg check` does)
- Route tables (all routes valid, targets reachable)
- NAT GW health (masquerade working, public IP reachable)
- Peering state (veth pairs exist, forwarding rules active)
- IP allocation consistency (IPAM bitmap matches actual NICs)
- Full path trace: given source VM and destination, trace the packet through SGs, routes, NAT, peering.

## Estimated Scope (Issues)

### Phase 1 — Security Groups (~18 issues)

1. SecurityGroup types + redb + default SG auto-creation
2. SecurityGroupRule types + redb
3. SG CLI — create, list, show, delete
4. SG rule CLI — add-rule, remove-rule, rules
5. SG attach/detach to VM (via NIC)
6. NetworkInterface (NIC) types + redb + auto-create on vm create
7. nftables: per-VM chain architecture + vmap dispatch
8. nftables: generate ingress chains from SG rules
9. nftables: generate egress chains from SG rules
10. nftables: named sets for SG-to-SG references
11. nftables: atomic apply with chain swap
12. vm create --sg integration + default SG auto-attach
13. sg check diagnostic command
14. Reconciliation: SG-aware drift detection
15. Migration: replace hardcoded rules with default SG
16. Deletion guards (SG, rules, NIC)
17. E2E: SG blocks unauthorized traffic
18. E2E: SG allows authorized traffic + sg check

### Phase 2 — Route Tables (~8 issues)

19. RouteTable types + redb + default table auto-creation
20. Route types + system/user/propagated origin
21. Route table CLI
22. Subnet association
23. Blackhole routes (nftables DROP)
24. Auto-add local routes on subnet creation
25. Route target validation (check resource state)
26. Deletion guards + reconciliation

### Phase 3 — NAT Gateway (~6 issues)

27. NatGateway types + redb + ResourceState lifecycle
28. NAT Gateway CLI — create, list, delete
29. Wire NAT GW into nftables masquerade
30. Route table integration — routes point to NAT GW ID
31. Remove implicit NAT — require explicit NAT GW
32. Deletion guards + state transitions

**Total: ~32 issues.**

## Commercial Value

This ADR delivers the network policy layer that differentiates a production cloud from a toy:

- **Per-VM firewalls**: operators define exactly which traffic reaches each VM. No more "everything can talk to everything."
- **SG-to-SG references**: model application architectures directly. "Web tier can reach DB tier on port 5432" — expressed as SG rules, not CIDR ranges that break when IPs change.
- **Explicit routing**: operators control traffic paths. No implicit internet egress — if you want NAT, you create a NAT Gateway. No surprises.
- **Route tables per subnet**: different subnets can have different routing policies. Frontend subnet routes through NAT GW. Database subnet has no internet route (blackhole 0.0.0.0/0).
- **Blackhole routes**: explicit traffic drops for compliance. "Traffic to 10.3.0.0/16 must be dropped" — one route, enforced at the network layer.
- **NAT Gateway as a resource**: visible, manageable, monitorable. Operators see exactly how internet egress works, which public IP is used for SNAT, and can replace/upgrade NAT GWs without touching VMs.
- **Deletion guards**: prevents accidental destruction of in-use resources. Production-grade safety.
- **Drift detection**: the reconciliation loop ensures the actual network state matches the desired state. If someone manually edits nftables, Syfrah corrects it within 30 seconds.

This is table-stakes functionality for any serious cloud platform. Without it, Syfrah cannot be used for multi-tier applications with different security postures.

## Rejected Alternatives

1. **SGs attached to VMs, not NICs**: rejected — does not model multi-NIC VMs correctly. Attaching to NICs is the industry standard (AWS, GCP, Azure). The UX shortcut (`--vm`) preserves simplicity for Phase 1.

2. **Implicit internet egress (SNAT always on)**: rejected — violates the principle of explicit resource management. An operator who creates a VPC should get an isolated network by default, not one that silently routes to the internet.

3. **No priority on SG rules**: rejected — without priority, rule ordering is undefined. This makes nftables chain generation non-deterministic and blocks future deny-rule support.

4. **ACLs instead of SGs**: rejected — ACLs are stateless and subnet-level. SGs are stateful and per-NIC. SGs are more intuitive for application-level policies ("allow port 5432 from web tier"). ACLs may be added later as a subnet-level defense-in-depth layer.

5. **Single nftables chain for all VMs**: rejected — O(n) rule matching per packet (where n = total rules across all VMs). Per-VM chains with vmap dispatch is O(1) dispatch + O(m) matching (where m = rules for one VM). Also, updating one VM's rules would require flushing and rebuilding the entire chain, risking transient rule gaps.

6. **No ResourceState — just create/delete**: rejected — network resources have asynchronous lifecycles. A NAT GW takes time to configure. Without intermediate states (Pending, Failed), the system cannot accurately represent reality or handle partial failures.

7. **Routes as strings ("nat", "peering:vpc-2") instead of resource IDs**: rejected — strings are fragile. If the NAT GW is renamed, routes break. Resource IDs are stable. Route target validation requires knowing the target resource's state, which requires an ID-based reference.

8. **Egress default-deny**: rejected for Phase 1 — too disruptive. Most operators expect outbound traffic to work. AWS and GCP default to all-egress-allowed. The locked egress model (all-allowed unless any egress rule exists) provides a safe default with an opt-in restriction path.

## References

- `handbook/adr-001-networking-roadmap.md` — networking foundation (VPC, Subnet, VXLAN, IPAM)
- `layers/overlay/README.md` — overlay design and nftables architecture
- `handbook/ARCHITECTURE.md` — global architecture
- Issue #861 — architecture debate and review
- AWS VPC Security Groups: https://docs.aws.amazon.com/vpc/latest/userguide/vpc-security-groups.html
- AWS NAT Gateway: https://docs.aws.amazon.com/vpc/latest/userguide/vpc-nat-gateway.html
- AWS Route Tables: https://docs.aws.amazon.com/vpc/latest/userguide/VPC_Route_Tables.html
- nftables wiki: https://wiki.nftables.org/
