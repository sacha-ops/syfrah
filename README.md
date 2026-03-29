# Syfrah

[![CI](https://github.com/sacha-ops/syfrah/actions/workflows/ci.yml/badge.svg)](https://github.com/sacha-ops/syfrah/actions/workflows/ci.yml)
[![E2E Tests](https://github.com/sacha-ops/syfrah/actions/workflows/e2e.yml/badge.svg)](https://github.com/sacha-ops/syfrah/actions/workflows/e2e.yml)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

An open-source WireGuard mesh networking tool for dedicated servers.

## What is Syfrah?

Syfrah connects dedicated servers from any provider (OVH, Hetzner, Scaleway, or others) into an encrypted WireGuard mesh network. Each pair of nodes establishes a point-to-point WireGuard tunnel, forming a full-mesh topology where every node can reach every other node over private, encrypted IPv6.

The mesh is fully decentralized with no central coordinator. An operator adds nodes one at a time through a manual peering process: the new node sends a join request, the operator approves it (or uses a pre-shared PIN), and the node receives the mesh credentials and peer list. All inter-node traffic is encrypted with WireGuard (Curve25519 + ChaCha20-Poly1305).

The long-term vision is to grow Syfrah into a full control plane that orchestrates compute, storage, and networking on top of the mesh. Today, it is a networking tool: it builds the encrypted fabric that everything else will run on.

## Status

| Layer | Crate | Status |
|---|---|---|
| **Core** | `syfrah-core` | Stable — types, crypto, IPv6 addressing |
| **State** (library) | `syfrah-state` | Stable — embedded persistence (redb), cross-cutting library used by all layers |
| **API** (library) | `syfrah-api` | Stable — error types, structured responses, cross-cutting library |
| **Fabric** | `syfrah-fabric` | Stable — WireGuard mesh, peering, daemon, CLI |
| Forge | — | Design phase |
| Compute | — | Design phase |
| Storage | — | Design phase |
| Overlay | — | Design phase |
| Control Plane | — | Design phase |
| IAM | — | Design phase |
| Org | — | Design phase |
| Products | — | Design phase |

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
                  ┌──────────────┴───────────────┐
                  │       syfrah-fabric           │
                  │                               │
                  │  peering.rs   TCP join/accept  │
                  │  daemon.rs    background loop  │
                  │  wg.rs        WireGuard iface  │
                  │  control.rs   Unix socket IPC  │
                  │  store.rs     state persist    │
                  │  events.rs    event log        │
                  └──────┬───────────┬────────────┘
                         │           │
              ┌──────────┴──┐  ┌─────┴──────────┐
              │ syfrah-core │  │  syfrah-state   │
              │             │  │                 │
              │ identity    │  │ redb wrapper    │
              │ addressing  │  │ ACID persistence│
              │ mesh types  │  │                 │
              │ crypto      │  │                 │
              └─────────────┘  └─────────────────┘
```

**Core** provides pure types with no I/O: node identities, WireGuard keypairs, mesh secrets, and deterministic IPv6 address derivation (ULA /48 prefix per mesh derived from the mesh secret, /128 address per node derived from SHA-256 of the WireGuard public key).

**State** wraps redb for crash-safe embedded persistence. Mesh state is stored in `~/.syfrah/fabric.redb`. A backward-compatible JSON export is maintained at `~/.syfrah/state.json` (debounced, best-effort).

**Fabric** is the main layer. It manages:
- A WireGuard interface (`syfrah0`) with a full-mesh topology
- A TCP peering protocol for manual node enrollment (PIN or interactive approval)
- Encrypted peer announcements (AES-256-GCM, key derived from mesh secret)
- A daemon loop with health checks (60s interval, 5-minute unreachable threshold)
- A reconciliation loop (30s interval) that keeps WireGuard config in sync with stored state
- Metrics: peers discovered, reconciliations, unreachable count, uptime

The CLI binary in `bin/syfrah` composes these crates and contains no logic of its own.

## Documentation

### Implemented layers

- [layers/fabric/README.md](layers/fabric/README.md) — Fabric: WireGuard mesh, peering protocol, security model, failure modes, scalability
- [layers/core/](layers/core/) — Core: types, crypto, addressing
- [layers/state/](layers/state/) — State: embedded persistence

### Architecture and handbook

- [handbook/ARCHITECTURE.md](handbook/ARCHITECTURE.md) — Full architecture vision and design principles
- [handbook/repository.md](handbook/repository.md) — Repository structure conventions
- [handbook/state-and-reconciliation.md](handbook/state-and-reconciliation.md) — State ownership and reconciliation design
- [handbook/cli.md](handbook/cli.md) — CLI command tree
- [handbook/testing.md](handbook/testing.md) — Testing strategy

## Roadmap

The layers below are architecturally designed with concept documentation but have no implementation yet. Each has a README in its `layers/` directory describing the planned design.

- **Forge** — per-node REST API for managing local resources
- **Compute** — KVM-based microVMs via Cloud Hypervisor
- **Storage** — S3-backed block devices (ZeroFS)
- **Overlay** — VXLAN, VPCs, security groups, private DNS
- **Control Plane** — Raft consensus + SWIM gossip, embedded on every node
- **IAM** — role-based access control and API keys
- **Org** — multi-tenant organization/project/environment model
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
