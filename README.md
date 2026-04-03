# Syfrah

[![CI](https://github.com/sacha-ops/syfrah/actions/workflows/ci.yml/badge.svg)](https://github.com/sacha-ops/syfrah/actions/workflows/ci.yml)
[![E2E Tests](https://github.com/sacha-ops/syfrah/actions/workflows/e2e.yml/badge.svg)](https://github.com/sacha-ops/syfrah/actions/workflows/e2e.yml)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

An open-source cloud platform that turns bare-metal servers into a programmable cloud.

## What is Syfrah?

Syfrah transforms dedicated servers from any provider (OVH, Hetzner, Scaleway, or others) into a unified cloud platform. It builds an encrypted WireGuard mesh between servers, then layers compute (Cloud Hypervisor VMs, container fallback), overlay networking (VXLAN, VPCs, security groups), a distributed control plane (Raft consensus + SWIM gossip), and multi-tenant organization management on top.

Nodes join the mesh through a manual peering process (PIN or interactive approval). Once connected, a node automatically detects and joins the Raft control plane cluster. The operator only needs to bootstrap the control plane on one node; all subsequent nodes auto-join on fabric join. All inter-node traffic is encrypted with WireGuard (Curve25519 + ChaCha20-Poly1305).

## Status

| Layer | Crate | Status |
|---|---|---|
| **Core** | `syfrah-core` | Stable — types, crypto, IPv6 addressing |
| **State** | `syfrah-state` | Stable — embedded persistence (redb) |
| **API** | `syfrah-api` | Stable — error types, structured responses |
| **Fabric** | `syfrah-fabric` | Stable — WireGuard mesh, peering, daemon |
| **Compute** | `syfrah-compute` | Stable — Cloud Hypervisor VMs, container fallback (crun + gVisor), image management |
| **Org** | `syfrah-org` | Stable — Org/Project/Environment, VPC, Subnet, Security Groups, Route Tables, NAT Gateway, IPAM |
| **Overlay** | `syfrah-overlay` | Stable — VXLAN, bridges, TAP/veth, nftables, FDB, ARP proxy |
| **Forge** | `syfrah-forge` | Stable — per-hypervisor REST API, reconciliation, capacity management, drain, Prometheus metrics |
| **Control Plane** | `syfrah-controlplane` | Stable — Raft consensus (openraft), SWIM gossip (foca), distributed scheduler, leader election |
| **Hypervisor** | (in `syfrah-org`) | Stable — Region/Zone/Hypervisor topology, auto-discovery, labels, taints, drain |
| Storage | — | Planned — ZeroFS + S3 block devices |
| IAM | — | Planned — role-based access control, API keys |
| Products | — | Planned — managed databases, load balancers |

## Install

### Pre-compiled binary (Linux / macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/sacha-ops/syfrah/main/scripts/install.sh | sh
```

### From crates.io

```bash
cargo install syfrah
```

### From source

```bash
git clone https://github.com/sacha-ops/syfrah.git
cd syfrah
cargo build --release
# Binary is at target/release/syfrah
```

Requires Rust stable (version pinned in [rust-toolchain.toml](rust-toolchain.toml)).

### Beta channel

To install the latest beta (built from `main`, pre-release, may contain breaking changes):

```bash
curl -fsSL https://raw.githubusercontent.com/sacha-ops/syfrah/main/scripts/install.sh | sh -s -- --beta
syfrah --version   # verify the installed version
```

See [handbook/releasing.md](handbook/releasing.md) for the full release strategy.

## Quick Start

```bash
# Server 1: create a mesh and start peering listener
syfrah fabric init --name my-cloud
syfrah fabric peering start --pin 4829

# Server 2: join the mesh
syfrah fabric join 203.0.113.1 --pin 4829

# Check status
syfrah fabric status
syfrah fabric peers
```

This creates an encrypted WireGuard mesh between the two servers. Each additional server repeats the `join` step. The operator approves every join, either manually or via PIN.

## How it works

```
                      ┌──────────────────────────────┐
                      │           CLI binary          │
                      │         (bin/syfrah)          │
                      └──────────────┬───────────────┘
                                     │
          ┌──────────────────────────┼──────────────────────────┐
          │                          │                          │
 ┌────────┴─────────┐  ┌────────────┴────────────┐  ┌─────────┴────────┐
 │  syfrah-forge    │  │   syfrah-controlplane   │  │   syfrah-org     │
 │                  │  │                          │  │                  │
 │  REST API        │  │  Raft consensus          │  │  Org/Project/Env │
 │  reconciliation  │  │  SWIM gossip             │  │  VPC/Subnet/SG   │
 │  capacity mgmt   │  │  distributed scheduler   │  │  IPAM, Hypervisor│
 └────────┬─────────┘  └────────────┬─────────────┘  └─────────┬───────┘
          │                         │                           │
 ┌────────┴─────────┐  ┌───────────┴────────────┐  ┌──────────┴───────┐
 │  syfrah-compute  │  │    syfrah-overlay      │  │  syfrah-fabric   │
 │                  │  │                         │  │                  │
 │  Cloud Hypervisor│  │  VXLAN, bridges, TAP    │  │  WireGuard mesh  │
 │  crun + gVisor   │  │  nftables, FDB, ARP    │  │  peering, daemon │
 └────────┬─────────┘  └───────────┬─────────────┘  └──────┬──────────┘
          │                        │                        │
          └────────────┬───────────┴────────────────────────┘
                       │
            ┌──────────┴──┐  ┌─────────────────┐
            │ syfrah-core │  │  syfrah-state   │
            │             │  │                 │
            │ identity    │  │ redb wrapper    │
            │ addressing  │  │ ACID persistence│
            │ crypto      │  │                 │
            └─────────────┘  └─────────────────┘
```

**Core** provides pure types with no I/O: node identities, WireGuard keypairs, mesh secrets, and deterministic IPv6 address derivation.

**State** wraps redb for crash-safe embedded persistence. All layers store data in `~/.syfrah/`.

**Fabric** manages the WireGuard mesh: encrypted tunnels, TCP peering protocol, health checks, reconciliation, and auto-join for the Raft control plane.

**Compute** runs workloads via Cloud Hypervisor VMs with container fallback (crun + gVisor). Handles image management, VM lifecycle, and resource tracking.

**Overlay** provides virtual networking: VXLAN tunnels, Linux bridges, TAP/veth devices, nftables rules, FDB entries, and ARP proxy for cross-hypervisor VM communication.

**Control Plane** implements distributed consensus (Raft via openraft), failure detection (SWIM gossip via foca), a distributed scheduler for VM placement, and leader election.

**Org** manages the multi-tenant hierarchy: Organizations, Projects, Environments, VPCs, Subnets, Security Groups, Route Tables, NAT Gateways, and IPAM. Also includes the Hypervisor model (Region/Zone/Hypervisor topology).

**Forge** exposes a per-hypervisor REST API for local resource management, capacity tracking, reconciliation, drain operations, and Prometheus metrics.

The CLI binary in `bin/syfrah` composes these crates and contains no logic of its own.

## Documentation

### Implemented layers

- [layers/core/](layers/core/) — Core: types, crypto, addressing
- [layers/state/](layers/state/) — State: embedded persistence
- [layers/fabric/README.md](layers/fabric/README.md) — Fabric: WireGuard mesh, peering, security model
- [layers/compute/](layers/compute/) — Compute: Cloud Hypervisor VMs, containers
- [layers/overlay/](layers/overlay/) — Overlay: VXLAN, VPCs, security groups
- [layers/controlplane/](layers/controlplane/) — Control Plane: Raft, gossip, scheduler
- [layers/org/](layers/org/) — Org: multi-tenant model, IPAM, hypervisor topology
- [layers/forge/](layers/forge/) — Forge: per-node REST API, capacity, metrics

### Architecture and handbook

- [handbook/ARCHITECTURE.md](handbook/ARCHITECTURE.md) — Full architecture vision and design principles
- [handbook/repository.md](handbook/repository.md) — Repository structure conventions
- [handbook/state-and-reconciliation.md](handbook/state-and-reconciliation.md) — State ownership and reconciliation design
- [handbook/cli.md](handbook/cli.md) — CLI command tree
- [handbook/testing.md](handbook/testing.md) — Testing strategy
- [handbook/adr-004-hypervisor-model.md](handbook/adr-004-hypervisor-model.md) — Hypervisor model (Region/Zone/Hypervisor/VM topology)

## Roadmap

The following layers are planned but not yet implemented:

- **Storage** — S3-backed block devices (ZeroFS), persistent volumes
- **IAM** — role-based access control, API keys, service accounts
- **Products** — managed databases, load balancers, composed from forge primitives

See [handbook/ARCHITECTURE.md](handbook/ARCHITECTURE.md) for the full design.

## Shell Completions

Syfrah supports tab completions for Bash, Zsh, and Fish:

```bash
# Bash
syfrah completions bash > /etc/bash_completion.d/syfrah

# Zsh — add ~/.zfunc to fpath in ~/.zshrc before compinit
syfrah completions zsh > ~/.zfunc/_syfrah

# Fish
syfrah completions fish > ~/.config/fish/completions/syfrah.fish
```

Run `syfrah completions --help` for detailed setup instructions.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

```bash
cargo build           # build all crates
cargo test            # run tests
cargo clippy          # lint
cargo run -- --help   # run the CLI
```

> **Note:** These are development commands for building from source, not the `syfrah` CLI. For CLI usage, see [Quick Start](#quick-start) above.

## Security

All inter-node traffic is encrypted by WireGuard (Curve25519 + ChaCha20-Poly1305). Peer announcements are additionally encrypted with AES-256-GCM. The TCP peering channel itself is not TLS-encrypted; join requests and responses are sent in plaintext. See the [fabric security model](layers/fabric/README.md#security-model) for the full threat model.

To report a security vulnerability, please email security@syfrah.dev.

## License

[Apache 2.0](LICENSE)
