# Syfrah

[![CI](https://github.com/sacha-ops/syfrah/actions/workflows/ci.yml/badge.svg)](https://github.com/sacha-ops/syfrah/actions/workflows/ci.yml)
[![Docs](https://github.com/sacha-ops/syfrah/actions/workflows/docs.yml/badge.svg)](https://sacha-ops.github.io/syfrah/)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

An open-source platform that turns bare-metal servers into a programmable cloud.

## What is Syfrah?

Syfrah transforms dedicated servers from any provider (OVH, Hetzner, Scaleway) into a unified cloud platform. It builds an encrypted WireGuard mesh between servers, then layers compute, networking, storage, and multi-tenant management on top.

Syfrah is a **CLI orchestrator** — not a daemon. It configures system services (WireGuard, systemd), then exits. The kernel does the heavy lifting.

## Status

| Layer | Crate | Status |
|---|---|---|
| **Core** | `syfrah-core` | Stable — types, crypto, addressing, resource framework, API generation |
| **State** | `syfrah-state` | Stable — embedded persistence (redb) |
| **Hypervisor** | `syfrah-hypervisor` | Stable — WireGuard mesh, peering, service lifecycle |
| Control Plane | — | Planned — Raft consensus, gossip, scheduler |
| Compute | — | Planned — Cloud Hypervisor VMs, containers |
| Overlay | — | Planned — VXLAN, VPCs, security groups |
| Storage | — | Planned — ZeroFS, S3-backed block devices |
| Org / IAM | — | Planned — multi-tenant, RBAC |

## Quick Start

```bash
# Server 1: create a mesh and start peering listener
syfrah hypervisor init --region eu --zone fsn1 --peering

# Server 2: join the mesh
syfrah hypervisor join --target 46.224.166.60 --pin G7CCZX --region eu --zone nbg1

# Check status
syfrah hypervisor status

# List all nodes
syfrah hypervisor list
```

## How it works

```
                      ┌──────────────────────────────┐
                      │           CLI binary          │
                      │         (bin/syfrah)          │
                      └──────────────┬───────────────┘
                                     │
                      ┌──────────────┴───────────────┐
                      │      syfrah-hypervisor       │
                      │                              │
                      │  fabric: WireGuard mesh      │
                      │  peering: TCP join protocol   │
                      │  service: systemd lifecycle   │
                      └──────────────┬───────────────┘
                                     │
               ┌─────────────────────┼─────────────────────┐
               │                                           │
    ┌──────────┴──┐                             ┌──────────┴──────┐
    │ syfrah-core │                             │  syfrah-state   │
    │             │                             │                 │
    │ resource    │                             │ redb wrapper    │
    │ framework   │                             │ ACID persistence│
    │ identity    │                             │                 │
    │ crypto      │                             └─────────────────┘
    │ addressing  │
    │ API gen     │
    └─────────────┘
```

**Core** provides pure types with no I/O: resource framework (generates CLI + API from a single definition), typed IDs (ULID), WireGuard keypairs, mesh secrets, IPv6 addressing.

**State** wraps redb for crash-safe embedded persistence. One database file per layer in `~/.syfrah/`.

**Hypervisor** is the central concept — every server is a hypervisor. It manages the WireGuard mesh (fabric), TCP peering protocol, and systemd service lifecycle.

The CLI binary in `bin/syfrah` composes these crates and contains no logic of its own.

## Documentation

- **[User Documentation](https://sacha-ops.github.io/syfrah/)** — Starlight site with guides for every module
- **[REST API Reference](https://sacha-ops.github.io/syfrah/rest/)** — Interactive Scalar UI (auto-generated from ResourceDefs)
- **[Rust API Reference](https://sacha-ops.github.io/syfrah/api/syfrah_core/)** — rustdoc for all crates

## Install

### From source

```bash
git clone https://github.com/sacha-ops/syfrah.git
cd syfrah
cargo build --release
# Binary is at target/release/syfrah
```

Requires Rust stable (version pinned in [rust-toolchain.toml](rust-toolchain.toml)).

## Contributing

```bash
cargo build           # build all crates
cargo test            # run tests (458+)
cargo clippy          # lint
cargo run -- --help   # run the CLI
```

## Security

All inter-node traffic is encrypted by WireGuard (Curve25519 + ChaCha20-Poly1305). Nodes join the mesh through a manual peering process — PIN or interactive approval. No automatic discovery.

## License

[Apache 2.0](LICENSE)
