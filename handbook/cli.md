---
tags: [cli, tooling, operations]
---
# CLI

## Two CLI Binaries

### `syfrah` — Server CLI (local)
Runs on hypervisors. Communicates via Unix domain socket to the local daemon.
No network, no auth (filesystem permissions are the auth).
Used by: admins with SSH access to hypervisors.

### `syfrah-cli` — Remote CLI (planned)
Runs on laptops/workstations. Communicates via gRPC/TLS to the Gateway.
Requires an API key (`syf_key_...`).
Used by: developers, platform ops, CI/CD pipelines.

Both CLIs offer the same operations. `syfrah-cli` is scoped by the API key's role and organization.

---

## Overview

`syfrah` is the single binary that operators use to manage the platform. The CLI is organized by **namespace**, where each namespace maps to an architectural layer and a directory in the codebase.

```
syfrah <namespace> <command> [flags]
```

The CLI communicates with the local daemon via a Unix domain socket (`~/.syfrah/control.sock`) for runtime operations, or directly reads/writes state for offline operations. The socket uses length-prefixed JSON frames (max 64 KB, 5 s read timeout); see [api-architecture.md](api-architecture.md#1-local-server-cli--daemon) for details.

> **Implementation status:** Only `syfrah fabric`, `syfrah state`, `syfrah update`, and `syfrah completions` are implemented. All other namespaces (`forge`, `org`, `vm`, `vpc`, etc.) are planned. See the command tree below for details.

## Command tree

Commands marked **(planned)** are not yet implemented.

```
syfrah
│
├── update                        Update syfrah to the latest release
│
├── completions                   Generate shell completions (bash, zsh, fish)
│
├── fabric                        Fabric mesh management
│   ├── init                      Create a new mesh
│   ├── join                      Join an existing mesh
│   ├── start                     Restart daemon from saved state
│   ├── stop                      Stop the daemon
│   ├── leave                     Leave the mesh, clear state
│   ├── status                    Show mesh and daemon status
│   ├── events                    Show the event log
│   ├── audit                     Show the security audit log
│   ├── peers                     List all mesh peers
│   │   ├── remove                Remove a peer from the mesh
│   │   └── update                Update a peer's endpoint
│   ├── token                     Show the mesh secret
│   ├── rotate                    Rotate the mesh secret
│   ├── diagnose                  Run diagnostic checks
│   ├── reload                    Reload config.toml without restart
│   ├── topology                  Show mesh topology by region/zone
│   ├── metrics                   Export Prometheus metrics
│   ├── hosts                     Print /etc/hosts entries for peers
│   ├── service                   Manage the systemd service
│   │   ├── install               Install and enable service
│   │   ├── uninstall             Disable and remove service
│   │   └── status                Show service status
│   ├── peering                   Manage join requests
│   │   ├── start                 Start accepting joins
│   │   ├── stop                  Stop accepting joins
│   │   ├── list                  List pending requests
│   │   ├── watch                 Watch for requests interactively
│   │   ├── accept                Accept a request
│   │   └── reject                Reject a request
│   └── zone                      Manage zone drain state
│       ├── drain                 Mark a zone as draining
│       ├── undrain               Restore a zone to active
│       └── status                Show zone health and drain status
│
├── state                         Inspect and manage layer state databases
│   ├── list                      List tables in a layer's state database
│   ├── get                       Get values from a table
│   └── drop                      Drop (delete) a layer's state database
│
├── forge                         Per-node debug and ops (planned)
│   ├── status                    This node's health and resources (planned)
│   ├── vms                       Cloud Hypervisor microVM processes on this node (planned)
│   ├── bridges                   Active bridges, VXLAN, TAP devices (planned)
│   ├── volumes                   Mounted ZeroFS volumes, cache stats (planned)
│   ├── nftables                  Active security group rules (planned)
│   ├── logs                      Tail daemon logs (planned)
│   └── drain                     Prepare node for maintenance (planned)
│
├── org                           Organization management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   └── delete                    (planned)
│
├── project                       Project management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   └── delete                    (planned)
│
├── env                           Environment management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   ├── update                    (planned)
│   └── destroy                   (planned)
│
├── vm                            Virtual machine management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   ├── start                     (planned)
│   ├── stop                      (planned)
│   ├── reboot                    (planned)
│   ├── delete                    (planned)
│   └── ssh                       (planned)
│
├── vpc                           VPC management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   ├── delete                    (planned)
│   └── peer                      (planned)
│
├── subnet                        Subnet management (planned)
│   ├── create                    (planned)
│   └── list                      (planned)
│
├── sg                            Security group management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   ├── add-rule                  (planned)
│   └── remove-rule               (planned)
│
├── volume                        Volume management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   ├── attach                    (planned)
│   ├── detach                    (planned)
│   ├── delete                    (planned)
│   └── snapshot                  (planned)
│
├── user                          User management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   └── disable                   (planned)
│
├── iam                           Role assignment (planned)
│   ├── assign                    (planned)
│   ├── list                      (planned)
│   └── revoke                    (planned)
│
├── apikey                        API key management (planned)
│   ├── create                    (planned)
│   ├── list                      (planned)
│   ├── rotate                    (planned)
│   └── delete                    (planned)
│
├── login                         Authenticate (planned)
└── logout                        Clear session (planned)
```

## Namespace mapping

Each CLI namespace maps to an architectural layer, a source of truth, and a directory in the codebase. Only **implemented** namespaces have working code today.

| Namespace | Layer | Source of truth | Code directory | Status |
|---|---|---|---|---|
| `fabric` | Fabric | Local state (redb) | `layers/fabric/src/cli/` | Implemented |
| `state` | State | Local state databases | `layers/state/src/cli/` | Implemented |
| `update` | — | GitHub releases | `bin/syfrah/src/update.rs` | Implemented |
| `completions` | — | — | `bin/syfrah/src/main.rs` | Implemented |
| `forge` | Forge (per-node) | Local reality (observed) | — | Planned |
| `org`, `project`, `env` | Organization | Raft (desired) | — | Planned |
| `vm` | Compute | Raft (desired) | — | Planned |
| `vpc`, `subnet`, `sg` | Overlay | Raft (desired) | — | Planned |
| `volume` | Storage | Raft (desired) | — | Planned |
| `user`, `iam`, `apikey` | IAM | Raft (desired) | — | Planned |
| `login`, `logout` | IAM | Local session | — | Planned |

## `update` — self-update

Downloads and installs the latest `syfrah` binary. By default, the daemon is automatically restarted after the update completes.

### `syfrah update`

```bash
syfrah update              # download and install latest, auto-restart daemon
syfrah update --check      # only check if an update is available
syfrah update --no-restart # update binary but skip daemon restart
syfrah update --force      # skip the active-peers confirmation prompt
```

| Flag | Default | Description |
|---|---|---|
| `--check` | false | Only check for update, don't install |
| `--no-restart` | false | Skip automatic daemon restart (prints manual instructions) |
| `--force` | false | Skip confirmation prompt when peers are connected |

When peers are connected, the update command prompts for confirmation before
restarting the daemon (peers will briefly lose connectivity). Pass `--force` to
skip this prompt — required for unattended use (cron, CI) since non-interactive
sessions reject the prompt by default.

## `fabric` — mesh management

Manages the WireGuard mesh. This is the first thing an operator uses.

### `syfrah fabric init`

```bash
syfrah fabric init --name my-cloud
syfrah fabric init --name my-cloud --node-name par-hv-1 --endpoint 51.210.x.x:51820
syfrah fabric init --name my-cloud -d   # daemon in background
```

| Flag | Default | Description |
|---|---|---|
| `--name` | required | Mesh name |
| `--node-name` | hostname | This node's name |
| `--port` | 51820 | WireGuard listen port |
| `--endpoint` | auto-detect | Public IP:port |
| `--peering-port` | port + 1 | TCP port for peering |
| `-d, --daemon` | foreground | Background mode |

### `syfrah fabric join`

```bash
syfrah fabric join 51.210.x.x
syfrah fabric join 51.210.x.x --pin 1234
syfrah fabric join 51.210.x.x --node-name fsn-hv-1 -d
```

| Flag | Default | Description |
|---|---|---|
| `target` | required | IP or IP:port of existing node |
| `--node-name` | hostname | This node's name |
| `--port` | 51820 | WireGuard listen port |
| `--endpoint` | auto-detect | Public IP:port |
| `--pin` | none | PIN for auto-accept |
| `-d, --daemon` | foreground | Background mode |

### `syfrah fabric start` / `stop` / `leave`

```bash
syfrah fabric start         # restart daemon from saved state
syfrah fabric start -d      # background
syfrah fabric stop          # stop the daemon
syfrah fabric leave         # leave mesh, clear all state
```

### `syfrah fabric status`

```bash
syfrah fabric status
```

```
Mesh:      my-cloud
Node:      par-hv-1
Mesh IPv6: fd12:3456:7800:a1b2:c3d4:e5f6:7890:abcd
Prefix:    fd12:3456:7800::/48
WG port:   51820
Peering:   port 51821
Daemon:    running (pid 12345)

Interface: syfrah0 (up)
WG peers:  3 configured, 3 with handshake
Traffic:   rx 1.2 MiB / tx 3.4 MiB

Metrics:
  Uptime:          2h 30m
  Peers discovered: 3
  WG reconciles:   12
```

### `syfrah fabric peers`

```bash
syfrah fabric peers
```

```
NAME               MESH IP                                  ENDPOINT             STATUS   HANDSHAKE   TRAFFIC
----------------------------------------------------------------------------------------------------------------
par-hv-2           fd12:3456:...:5555                       51.210.x.y:51820    active       5s ago  1.2K↓ 3.4K↑
fsn-hv-1           fd12:3456:...:eeee                       88.198.x.y:51820    active      12s ago  4.5K↓ 2.1K↑
```

### `syfrah fabric token` / `rotate`

```bash
syfrah fabric token          # display mesh secret
syfrah fabric rotate         # generate new secret (daemon must be stopped)
```

### `syfrah fabric peering`

```bash
syfrah fabric peering                    # interactive mode
syfrah fabric peering --pin 1234         # auto-accept mode
syfrah fabric peering start --pin 1234   # non-interactive
syfrah fabric peering stop
syfrah fabric peering list
syfrah fabric peering accept abc12345
syfrah fabric peering reject abc12345 --reason "unknown"
```

### `syfrah fabric events`

```bash
syfrah fabric events          # show the event log
syfrah fabric events --json   # output as JSON
```

### `syfrah fabric diagnose`

```bash
syfrah fabric diagnose        # run diagnostic checks on the fabric
```

### `syfrah fabric service`

```bash
syfrah fabric service install     # install and enable the systemd service
syfrah fabric service uninstall   # disable and remove the systemd service
syfrah fabric service status      # show systemd service status
```

## `state` — inspect and manage layer state databases

The `state` namespace provides low-level access to the redb state databases used by each layer. Useful for debugging and recovery.

### `syfrah state list`

```bash
syfrah state list fabric          # list tables in the fabric state database
syfrah state list nonexistent     # error: no database for this layer
```

### `syfrah state get`

```bash
syfrah state get fabric peers     # get all values from the "peers" table
syfrah state get fabric config    # get all values from the "config" table
syfrah state get fabric peers my-key  # get a specific entry by key
```

| Argument | Required | Description |
|---|---|---|
| `layer` | yes | Layer name (e.g., `fabric`) |
| `table` | yes | Table name (e.g., `peers`, `config`, `metrics`) |
| `key` | no | Specific key to look up. If omitted, all entries are printed. |

The `metrics` table is special-cased: values are plain integers, not JSON.

### `syfrah state drop`

```bash
syfrah state drop fabric --force  # delete the fabric state database
```

| Flag | Default | Description |
|---|---|---|
| `--force` | false | Skip the interactive confirmation prompt |

**Warning:** `syfrah state drop` permanently deletes a layer's state database. Without `--force`, an interactive `[y/N]` prompt is shown.

## `forge` — per-node debug and ops (planned)

> **Not yet implemented.** The following describes the planned design.

Exposes the **observed state** of a specific node. While `syfrah vm list` shows what Raft thinks (desired state), `syfrah forge vms` shows what is actually running on the node (reality).

All forge commands target the **local node** by default. Use `--node` to target a remote node via the fabric.

### `syfrah forge status`

```bash
syfrah forge status
syfrah forge status --node fsn-hv-1
```

```
Node:      par-hv-1
Health:    active
Uptime:    5d 12h

Resources:
  vCPU:    8 / 32 used
  Memory:  16384 / 65536 MB used
  Disk:    200 / 1000 GB used

VMs:       4 running, 1 stopped
Bridges:   2 active (VNI 100, VNI 205)
Volumes:   5 mounted
Cache:     142 GB / 200 GB (71% hit rate)
```

### `syfrah forge vms`

```bash
syfrah forge vms
syfrah forge vms --node par-hv-2
```

```
VM ID          NAME       vCPU  MEM(MB)  STATUS    PID     UPTIME
─────────────────────────────────────────────────────────────────
vm-a1b2c3      web-1      2     4096     running   14523   2d 5h
vm-d4e5f6      web-2      2     4096     running   14601   2d 5h
vm-g7h8i9      db-primary 4     8192     running   14780   5d 12h
vm-j0k1l2      worker-1   1     1024     stopped   -       -
```

Shows actual Cloud Hypervisor microVM processes, not Raft desired state.

### `syfrah forge bridges`

```bash
syfrah forge bridges
```

```
BRIDGE      VNI    SUBNET           TAP DEVICES        FDB ENTRIES
────────────────────────────────────────────────────────────────────
br-100      100    10.0.1.0/24      tap-a1b2, tap-d4e5  6
br-205      205    10.0.2.0/24      tap-g7h8             3
```

### `syfrah forge volumes`

```bash
syfrah forge volumes
```

```
VOLUME         SIZE     VM           NBD DEVICE   CACHE     S3 BUCKET
──────────────────────────────────────────────────────────────────────
vol-abc123     100 GB   web-1        /dev/nbd0    45 GB     syfrah-eu-west
vol-def456     100 GB   db-primary   /dev/nbd1    89 GB     syfrah-eu-west
vol-ghi789     50 GB    (detached)   -            -         syfrah-eu-west
```

### `syfrah forge nftables`

```bash
syfrah forge nftables
syfrah forge nftables --vm web-1
```

Shows the actual nftables rules applied on the host. Useful for debugging security group issues.

### `syfrah forge logs`

```bash
syfrah forge logs              # tail local daemon logs
syfrah forge logs --follow     # continuous tail
syfrah forge logs --lines 100  # last 100 lines
```

### `syfrah forge drain`

```bash
syfrah forge drain                    # mark this node as draining
syfrah forge drain --node fsn-hv-1    # remote node
```

Marks the node as `Draining` in Raft. The scheduler stops placing new VMs here. Existing VMs continue running until manually migrated or stopped.

## Future namespaces (planned)

> **Not yet implemented.** The following describes the planned CLI design for future layers.

### `org`, `project`, `env`

```bash
syfrah org create acme --admin-email alice@example.com
syfrah project create backend-api --org acme
syfrah env create production --project backend-api --deletion-protection
syfrah env create feat/auth --project backend-api --ttl 48h
syfrah env list --project backend-api
syfrah env destroy feat/auth
```

### `vm`

```bash
syfrah vm create --env production --name web-1 --vcpu 2 --memory 4096 --image ubuntu-24.04
syfrah vm list --env production
syfrah vm start web-1
syfrah vm stop web-1
syfrah vm delete web-1
syfrah vm ssh web-1
```

### `vpc`, `subnet`, `sg`

```bash
syfrah vpc create prod --project backend-api --cidr 10.0.0.0/16
syfrah subnet create web --vpc prod --cidr 10.0.1.0/24
syfrah sg create web-sg --vpc prod
syfrah sg add-rule web-sg --ingress --tcp --port 443 --from 0.0.0.0/0
```

### `volume`

```bash
syfrah volume create data --env production --size 100
syfrah volume attach data --vm web-1
syfrah volume snapshot data --name daily
```

### `user`, `iam`, `apikey`

```bash
syfrah user create --email bob@example.com --name "Bob"
syfrah iam assign bob@example.com --role developer --project backend-api
syfrah apikey create --project backend-api --role developer --name ci
syfrah login --email alice@example.com
```

## Code structure

CLI commands live inside their layer crate. The binary (`bin/syfrah/`) composes them.

```
bin/syfrah/src/
├── main.rs              Binary — composes all layers, defines clap tree
└── update.rs            Self-update logic

layers/fabric/src/cli/
├── mod.rs               CLI command modules
├── init.rs              syfrah fabric init
├── join.rs              syfrah fabric join
├── start.rs             syfrah fabric start
├── stop.rs              syfrah fabric stop
├── leave.rs             syfrah fabric leave
├── status.rs            syfrah fabric status
├── events.rs            syfrah fabric events
├── audit.rs             syfrah fabric audit
├── peers.rs             syfrah fabric peers
├── peers_remove.rs      syfrah fabric peers remove
├── peers_update.rs      syfrah fabric peers update
├── token.rs             syfrah fabric token
├── rotate.rs            syfrah fabric rotate
├── diagnose.rs          syfrah fabric diagnose
├── reload.rs            syfrah fabric reload
├── topology.rs          syfrah fabric topology
├── metrics.rs           syfrah fabric metrics
├── hosts.rs             syfrah fabric hosts
├── service.rs           syfrah fabric service {install,uninstall,status}
├── peering.rs           syfrah fabric peering {start,stop,list,watch,accept,reject}
└── zone.rs              syfrah fabric zone {drain,undrain,status}

layers/state/src/cli/
├── mod.rs               StateCommand enum + dispatch
├── list.rs              syfrah state list
├── get.rs               syfrah state get
└── drop.rs              syfrah state drop
```

### Convention

Every command file exports:
- `pub struct Args` — clap args/flags (where applicable)
- `pub async fn run(args: Args) -> Result<()>` — implementation

Every namespace `mod.rs` exports:
- `pub enum {Namespace}Command` — clap subcommand enum
- `pub async fn run(cmd: {Namespace}Command) -> Result<()>` — dispatch

Adding a new command:
1. Create `layers/{layer}/src/cli/{command}.rs` with `Args` + `run()`
2. Add `mod {command}` + variant in `layers/{layer}/src/cli/mod.rs`
3. Wire it up in `bin/syfrah/src/main.rs`

## Design principles

- **One binary, all operations.** Everything is `syfrah`.
- **Namespaced by resource.** `syfrah vm`, `syfrah vpc`, `syfrah fabric`.
- **`fabric` for mesh ops.** The infrastructure bootstrap layer.
- **`forge` for node debug.** Observed state of a specific node.
- **Everything else for cluster ops.** Desired state via the control plane.
- **Repo mirrors CLI.** Directory = namespace, file = command.
- **Human-readable by default.** Tables, aligned columns. `--json` flag for machines (future).
- **No emojis, no colors.** Infrastructure tool, used over SSH.

## Files

All local state lives in `~/.syfrah/`:

| File | Purpose |
|---|---|
| `state.json` | Mesh config, peers, metrics (0600) |
| `control.sock` | Unix socket for CLI ↔ daemon (0600) |
| `daemon.pid` | Running daemon PID |
| `syfrah.log` | Daemon logs (auto-rotated at 10 MB) |
| `syfrah.log.old` | Previous log file |
| `session.json` | Login session token (future) |

## Relationship to other concepts

```
    ┌──────────────────────────────────────────────────────┐
    │                                                      │
    │   CLI              ◄── this document                 │
    │   syfrah binary: fabric + forge + cloud ops          │
    │                                                      │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │   Control Plane    ◄── docs/concepts/control-plane.md│
    │   Cloud commands talk to Raft via HTTP on fabric     │
    │                                                      │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │   Forge            ◄── docs/concepts/forge.md        │
    │   forge commands query local node state              │
    │                                                      │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │   Fabric           ◄── docs/concepts/fabric.md       │
    │   fabric commands manage the WireGuard mesh          │
    │                                                      │
    └──────────────────────────────────────────────────────┘
```
