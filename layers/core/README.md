# syfrah-core

Core building blocks for the Syfrah cloud platform. Contains:

- **Resource framework** — declarative CLI generation from resource definitions
- **Typed IDs** — ULID-backed, sortable, validated resource identifiers
- **Error types** — unified error model with codes, HTTP mapping, retry hints
- **Validation** — shared input validators for names, CIDRs, ports, durations, IPs, MACs, URLs
- **Transport** — Unix socket protocol between CLI and daemon (framing, router, client/server)
- **Config** — `~/.syfrah/config.toml` parsing with defaults, validation, hot-reload support

---

## Typed IDs (`syfrah_core::id`)

Every resource has a generated, immutable ID. IDs are the primary key everywhere — Raft, stores, API, logs. Names are for humans, IDs are for machines.

### Format

```
{prefix}-{26-char-ULID}
vpc-01JAXZ7KG8MN2P4Q6R9S0T1V2
```

### Properties

| Property | How |
|----------|-----|
| **Sortable** | ULID encodes timestamp first → lexicographic order = chronological order |
| **Unique** | 48-bit timestamp + 80-bit random per millisecond |
| **Typed** | `VpcId`, `OrgId`, `HypervisorId` — compiler catches misuse |
| **Validated** | `FromStr` rejects malformed IDs; `From<&str>` is unchecked (for deserialization) |
| **Introspectable** | `created_at_ms()` extracts creation timestamp from any ID |

### 17 ID types

`OrgId`, `ProjectId`, `EnvId`, `VpcId`, `SubnetId`, `SgId`, `HypervisorId`, `VmId`, `VolumeId`, `SnapshotId`, `NicId`, `NatGwId`, `RouteTableId`, `RuleId`, `PeeringId`, `NodeId`, `MeshId`

### Usage

```rust
use syfrah_core::id::VpcId;

// Generate
let id = VpcId::generate();
assert!(id.as_str().starts_with("vpc-"));

// Parse with validation
let parsed: VpcId = "vpc-01JAXZ7KG8MN2P4Q6R9S0T1V2".parse().unwrap();
assert!("org-01JAXZ7KG8MN2P4Q6R9S0T1V2".parse::<VpcId>().is_err()); // wrong prefix

// Introspect
let ts = id.created_at_ms().unwrap(); // milliseconds since epoch
let ulid = id.ulid_part().unwrap();   // raw ULID string

// Classify input
VpcId::looks_like_id("vpc-01JAX...");  // true — it's an ID
VpcId::looks_like_id("my-vpc");        // false — it's a name

// Works in HashMaps (Deref<str> + Borrow<str>)
use std::collections::HashMap;
let mut map: HashMap<VpcId, String> = HashMap::new();
map.insert(id.clone(), "my-vpc".into());
assert!(map.contains_key(id.as_str())); // lookup by &str
```

### Serde

IDs serialize as plain strings (transparent), so JSON looks like:
```json
{"id": "vpc-01JAXZ7KG8MN2P4Q6R9S0T1V2", "name": "my-vpc"}
```

Not `{"id": {"VpcId": "..."}}`.

---

## Error Types (`syfrah_core::error`)

Unified error model for the entire platform. Every layer returns `SyfrahError` so errors are consistent, actionable, and machine-parseable.

### Structure

```rust
use syfrah_core::error::{SyfrahError, ErrorCode};

let err = SyfrahError::not_found("vpc", "my-vpc")
    .with_suggestion("List available VPCs with: syfrah vpc list")
    .with_context("zone", "fsn1");

// Typed code — compiler catches typos
assert_eq!(err.code, ErrorCode::ResourceNotFound);

// HTTP mapping for API responses
assert_eq!(err.code.http_status(), 404);

// Retry hint for callers
assert!(!err.code.is_retryable());
```

### Error codes

| Code | HTTP | Retryable | When |
|------|------|-----------|------|
| `ResourceNotFound` | 404 | no | Resource doesn't exist |
| `ResourceAlreadyExists` | 409 | no | Duplicate create |
| `ValidationError` | 400 | no | Bad input |
| `InvalidName` | 400 | no | Name format violation |
| `PermissionDenied` | 403 | no | Not authorized |
| `Conflict` | 409 | no | State prevents operation |
| `PreconditionFailed` | 412 | no | Something must be done first |
| `AmbiguousName` | 400 | no | Multiple resources match |
| `RateLimited` | 429 | **yes** | Too many requests |
| `InternalError` | 500 | no | Unexpected server error |
| `NotImplemented` | 501 | no | Feature not available |
| `DaemonUnreachable` | 503 | **yes** | Daemon not running |
| `Timeout` | 504 | **yes** | Operation timed out |
| `NetworkError` | 502 | **yes** | Network failure |
| `StorageError` | 500 | no | Storage backend error |

### Common constructors

```rust
SyfrahError::not_found("vpc", "web")
SyfrahError::already_exists("org", "acme")
SyfrahError::validation("CIDR must include prefix length")
SyfrahError::invalid_name("MY_VPC", "must be lowercase")
SyfrahError::conflict("subnet", "web", "has active VMs")
SyfrahError::precondition("storage not configured for zone fsn1")
    .with_suggestion("Run: syfrah storage configure --zone fsn1 ...")
SyfrahError::daemon_unreachable()  // includes suggestion automatically
SyfrahError::ambiguous("vpc", "web", vec![("vpc-01AAA", "org: acme"), ...])
SyfrahError::timeout("vm create", 60)
SyfrahError::rate_limited()
```

### Dual formatting

```rust
// CLI mode (default):
// Error: vpc 'web' not found
// List available VPCs with: syfrah vpc list
err.format_cli();

// JSON mode (--json or API):
// {"code": "RESOURCE_NOT_FOUND", "message": "vpc 'web' not found", "suggestion": "..."}
err.format_json();
```

### Auto-conversion from common errors

```rust
// io::Error → SyfrahError (maps ErrorKind to correct code)
let err: SyfrahError = io_error.into();  // NotFound → 404, PermissionDenied → 403, etc.

// serde_json::Error → SyfrahError
let err: SyfrahError = json_error.into();  // → InternalError

// Convenience alias
fn my_fn() -> SyfrahResult<()> {
    Err(SyfrahError::not_found("vpc", "web"))
}
```

---

## Validation (`syfrah_core::validate`)

Shared input validators. Every user-facing input passes through these — never duplicated across layers.

```rust
use syfrah_core::validate;

// Resource names: 3-63 chars, lowercase, alphanumeric + hyphens, DNS-label compliant
validate::name("my-vpc")?;      // ok
validate::name("MY_VPC")?;      // Err: invalid character
validate::name("ab")?;          // Err: too short

// CIDR blocks: validates octets, prefix length, and network address
validate::cidr("10.1.0.0/16")?;     // ok
validate::cidr("10.1.1.0/16")?;     // Err: host bits not zero, suggests 10.1.0.0/16
validate::cidr("10.0.0.0/33")?;     // Err: prefix > 32

// Ports
validate::port(443)?;               // ok
validate::port(0)?;                  // Err
validate::port_str("80")?;          // ok, returns u16

// Regions and zones
validate::region("eu-west")?;       // ok
validate::zone("fsn1")?;            // ok

// Labels (key=value)
let (k, v) = validate::label("env=prod")?;  // ok

// Sizes and compute
validate::size_gb(50)?;             // ok (1-65536 GB)
validate::memory_mb(2048)?;         // ok (128 MB - 1 TB)
validate::vcpus(4)?;                // ok (1-256)

// Durations: "30s", "5m", "2h", "7d" → returns seconds
let secs = validate::duration("2h")?;  // 7200
```

```rust
// IP addresses
validate::ipv4("10.0.0.1")?;        // ok, returns [u8; 4]
validate::ipv6("fd01::1")?;          // ok
validate::ip_addr("10.0.0.1")?;      // ok (IPv4 or IPv6)

// IPv6 CIDR
validate::cidr_v6("fd00::/48")?;     // ok
validate::cidr_any("10.0.0.0/8")?;   // ok — dispatches to v4 or v6

// MAC addresses
validate::mac_address("aa:bb:cc:dd:ee:ff")?;  // ok, returns [u8; 6]

// Hostnames (RFC 1123)
validate::hostname("my-server.example.com")?;  // ok

// URLs
validate::url("https://s3.example.com")?;      // ok

// Endpoints (host:port, including IPv6)
let (host, port) = validate::endpoint("10.0.0.1:8080")?;
let (host, port) = validate::endpoint("[fd01::1]:7200")?;

// Port ranges
let (start, end) = validate::port_range("8080-8090")?;

// Email
validate::email("user@example.com")?;

// Paths
validate::path_exists("/tmp")?;
validate::file_exists("/etc/hosts")?;
```

All validators return `SyfrahError` with actionable messages:
```
Error: invalid CIDR '10.1.1.0/16': host bits must be zero. Did you mean 10.1.0.0/16?
Error: invalid MAC address 'aa:bb': must be 6 hex pairs separated by colons
Error: invalid endpoint 'noport': must be in format HOST:PORT or IP:PORT
```

---

## Transport (`syfrah_core::transport`)

Unix domain socket protocol between CLI and daemon. Generic, resource-kind-based routing — adding a new resource doesn't require changing the protocol.

### Protocol

```text
CLI                              Daemon
 │                                 │
 │── [4 bytes len][JSON Request] ─→│
 │                                 │── Router dispatches by kind
 │←─ [4 bytes len][JSON Response] ─│
 │                                 │
 └── close ────────────────────────┘
```

### Request / Response

```rust
use syfrah_core::transport::{Request, Response};

// CLI builds a request
let req = Request::resource("vpc", "create", Some("my-vpc".into()), fields)
    .with_scope("org", "acme");

// Daemon returns a response
let resp = Response::ok(serde_json::json!({"name": "my-vpc", "cidr": "10.0.0.0/16"}));
let resp = Response::err(SyfrahError::not_found("vpc", "web"));
let resp = Response::ok_message("vpc 'my-vpc' deleted.");
let resp = Response::ok_empty();
```

### Client (CLI side)

```rust
use syfrah_core::transport::{send_request, socket_path};

let resp = send_request(&socket_path(), &req).await?;
// If daemon is not running → SyfrahError::daemon_unreachable()
```

### Server (daemon side)

```rust
use syfrah_core::transport::{Router, RequestHandler, Request, Response, bind_listener};

struct VpcHandler;

#[async_trait::async_trait]
impl RequestHandler for VpcHandler {
    async fn handle(&self, req: Request, caller_uid: Option<u32>) -> Response {
        match req.operation.as_str() {
            "create" => Response::ok(serde_json::json!({"name": req.name})),
            "list" => Response::ok(serde_json::json!([])),
            _ => Response::err(SyfrahError::not_implemented(&req.operation)),
        }
    }
}

let mut router = Router::new();
router.register("vpc", VpcHandler);
router.register("fabric", FabricHandler);

// Accept loop
let listener = bind_listener(&socket_path())?;
loop {
    let (stream, _) = listener.accept().await?;
    let req: Request = read_message(&mut stream).await?;
    let resp = router.dispatch(req, caller_uid).await;
    write_message(&mut stream, &resp).await?;
}
```

### Design choices

- **Generic routing by string kind** — not an enum per layer. Adding a resource = registering a handler, no protocol changes.
- **Length-prefixed JSON** — simple, debuggable, max 1 MB.
- **Restrictive socket permissions** — 0o600, owner-only.
- **Error responses are structured** — `SyfrahError` with code, message, suggestion.
- **No streaming** — one request, one response, close. Keeps it simple.

---

## Config (`syfrah_core::config`)

Configuration from `~/.syfrah/config.toml` with env var and CLI overrides. All durations are human-readable (`"60s"`, `"5m"`, `"2h"`).

### Priority (highest wins)

```
CLI flags  →  env vars (SYFRAH_*)  →  config.toml  →  defaults
```

### Usage

```rust
use syfrah_core::config::Config;

let config = Config::load()?;  // file → env → validate

config.daemon.health_check_interval  // "60s"
config.wireguard.interface_name      // "syfrah0"
config.logging.level                 // "info"

// Parse duration to seconds
Config::duration_secs("5m")?  // 300
```

### Env var overrides

```bash
SYFRAH_LOG_LEVEL=debug syfrah fabric start     # overrides logging.level
SYFRAH_WG_PORT=9999 syfrah fabric start        # overrides wireguard.listen_port
```

| Env var | Config field |
|---------|-------------|
| `SYFRAH_LOG_LEVEL` | `logging.level` |
| `SYFRAH_LOG_FORMAT` | `logging.format` |
| `SYFRAH_LOG_FILE` | `logging.file` |
| `SYFRAH_WG_INTERFACE` | `wireguard.interface_name` |
| `SYFRAH_WG_PORT` | `wireguard.listen_port` |
| `SYFRAH_HEALTH_INTERVAL` | `daemon.health_check_interval` |
| `SYFRAH_CACHE_MEMORY_MB` | `storage.cache_memory_mb` |

### CLI overrides

```rust
let mut overrides = HashMap::new();
overrides.insert("logging.level".into(), "debug".into());
config.apply_overrides(&overrides);
```

### Validation

All values are validated after loading. Invalid values = hard error before daemon starts:

```
Error: logging.level 'banana' is invalid. Must be one of: trace, debug, info, warn, error
Error: daemon.health_check_interval 'nope' is invalid (use e.g., 60s, 1m, 5m)
Error: wireguard.listen_port cannot be 0
```

Cross-field warnings (non-fatal):
```
Warning: storage: both cache_memory_mb and cache_disk_gb are 0 — no caching at all
```

### Properties

- **Optional file** — missing = all defaults
- **Partial config** — override only what you need
- **Unknown keys ignored** — forward-compatible
- **Human durations** — `"60s"`, `"5m"`, `"2h"`, `"7d"` everywhere
- **Schema version** — `config_version` field for future migrations
- **File permissions** — saved as 0o600 (owner-only)
- **Validated** — bad values caught at load time, not runtime

---

## Resource Framework (`syfrah_core::resource`)

The declarative resource framework that powers all of Syfrah's CLI (and future API). Instead of writing CLI commands by hand, you define **what a resource is** and the framework generates everything else.

## Why

Every cloud provider CLI has the same problem: hundreds of commands that should behave identically but don't. `list` sometimes has `--json`, sometimes doesn't. `delete` sometimes asks for confirmation, sometimes doesn't. Error messages vary wildly. Adding a new resource means copying 500 lines of CLI boilerplate and hoping you got it right.

This framework solves that. You describe your resource once, and the CLI is generated automatically with guaranteed consistency.

## How it works

```
ResourceDef (your definition)
     │
     ├──→ CLI Generator ──→ clap Commands (automatic)
     ├──→ Dispatcher ──→ extract, validate, call handler, render (automatic)
     ├──→ Renderer ──→ tables, detail views, JSON (automatic)
     └──→ Conformance tests ──→ compile-time guarantees (automatic)
```

A `ResourceDef` has 5 parts:

| Part | What it describes | What it controls |
|------|-------------------|-----------------|
| **Identity** | Kind, name, aliases | Top-level `syfrah <kind>` command |
| **Scope** | Parent resources, uniqueness | `--org`, `--vpc` flags, name resolution |
| **Schema** | Fields, types, mutability | `create` flags, `update` patch fields |
| **Operations** | CRUD + custom actions, constraints | Subcommands, validation, confirmation |
| **Presentation** | Table columns, detail fields, formats | `list` output, `get` output, `--json` |

## Quick start

### 1. Define your resource

```rust
use syfrah_core::resource::*;

fn vpc_resource() -> ResourceDef {
    ResourceDef::build("vpc", "Virtual Private Cloud")
        .plural("vpcs")
        .alias("network")
        .parent("org", "--org", "Organization")
        .field(FieldDef::cidr("cidr", "CIDR block").with_default("10.1.0.0/16"))
        .field(FieldDef::flag("shared", "Create a shared VPC"))
        .field(FieldDef::string("description", "VPC description").mutable())
        .crud()
        .action("peer", "Create a peering between two VPCs")
            .op(|op| op
                .with_arg(OperationArg::required("from", FieldDef::resource_ref("from", "Source VPC", "vpc")))
                .with_arg(OperationArg::required("to", FieldDef::resource_ref("to", "Destination VPC", "vpc")))
            )
        .column("NAME", "name")
        .column("CIDR", "cidr")
        .column("OWNER", "owner")
        .column_def(ColumnDef::new("SHARED", "shared").with_format(DisplayFormat::YesNo))
        .column_def(ColumnDef::new("CREATED", "created_at").with_format(DisplayFormat::Timestamp))
        .detail_section(None, vec![
            DetailField::new("Name", "name"),
            DetailField::new("ID", "id"),
            DetailField::new("CIDR", "cidr"),
            DetailField::new("Shared", "shared").with_format(DisplayFormat::YesNo),
            DetailField::new("Created", "created_at").with_format(DisplayFormat::Timestamp),
        ])
        .empty_message("No VPCs found. Create one with: syfrah vpc create <name> --org <org>")
        .done()
}
```

### 2. Register it with a handler

```rust
use syfrah_core::resource::*;

fn register_vpc(registry: &mut ResourceRegistry) {
    let handler: HandlerFn = Box::new(|req: OperationRequest| {
        Box::pin(async move {
            match req.operation.as_str() {
                "create" => {
                    let name = req.name.unwrap_or_default();
                    // ... send to daemon via control socket ...
                    Ok(OperationResponse::Resource(serde_json::json!({
                        "name": name,
                        "cidr": req.fields.get("cidr").unwrap_or(&"10.1.0.0/16".into()),
                    })))
                }
                "list" => {
                    // ... query daemon ...
                    Ok(OperationResponse::ResourceList(vec![]))
                }
                "delete" => {
                    let name = req.name.unwrap_or_default();
                    // ... send delete to daemon ...
                    Ok(OperationResponse::Message(format!("VPC '{name}' deleted.")))
                }
                _ => Ok(OperationResponse::None),
            }
        })
    });

    registry.register(ResourceRegistration {
        def: vpc_resource(),
        handler,
    });
}
```

### 3. That's it

The framework generates this CLI automatically:

```
$ syfrah vpc --help
Virtual Private Cloud

Usage: syfrah vpc [COMMAND]

Commands:
  create  Create a new resource
  list    List resources
  get     Get resource details
  delete  Delete a resource
  peer    Create a peering between two VPCs
  help    Print this message or the help of the given subcommand(s)

$ syfrah vpc create my-vpc --org acme --cidr 10.2.0.0/16
  Name:            my-vpc
  CIDR:            10.2.0.0/16
vpc 'my-vpc' created.

$ syfrah vpc list --json
[{"name": "my-vpc", "cidr": "10.2.0.0/16", ...}]

$ syfrah vpc delete my-vpc --org acme
Delete vpc 'my-vpc'? This cannot be undone. [y/N] y
vpc 'my-vpc' deleted.
```

Every `list` has `--json`. Every `delete` has `--yes`. Every `create` has a positional `<NAME>`. Every table is formatted identically. No exceptions, no forgetting.

## Architecture

### ResourceDef

The single source of truth. Everything is derived from this.

```rust
pub struct ResourceDef {
    pub identity: ResourceIdentity,     // who
    pub scope: ScopeDef,                // where in the hierarchy
    pub schema: ResourceSchema,         // what fields
    pub operations: Vec<OperationDef>,  // what you can do
    pub presentation: PresentationDef,  // how it looks
}
```

### Identity

```rust
pub struct ResourceIdentity {
    pub kind: &'static str,         // "vpc" — internal key
    pub cli_name: &'static str,     // "vpc" — what the user types
    pub plural: &'static str,       // "vpcs" — for messages
    pub description: &'static str,  // help text
    pub aliases: &'static [&'static str],  // ["network"]
}
```

### Scope

Defines where a resource lives in the hierarchy and how names are unique.

```rust
pub struct ScopeDef {
    pub parents: Vec<ParentRef>,        // parent resources
    pub uniqueness: UniquenessScope,    // where names must be unique
}

// Examples:
ScopeDef::global()                              // org — globally unique
ScopeDef::within("org", "--org", "Organization") // project — unique within org
// subnet — unique within vpc, with multiple parents:
ScopeDef {
    parents: vec![
        ParentRef { kind: "vpc", flag: "--vpc", ... },
        ParentRef { kind: "env", flag: "--env", ... },
    ],
    uniqueness: UniquenessScope::WithinParent("vpc"),
}
```

### Schema

The fields a resource has. Controls what flags appear on `create` and `update`.

```rust
FieldDef::string("name", "Resource name")           // --name <STRING>
FieldDef::cidr("cidr", "CIDR block")                // --cidr <CIDR>
FieldDef::flag("shared", "Make it shared")           // --shared (boolean)
FieldDef::integer("vcpus", "Number of vCPUs")        // --vcpus <INT>
FieldDef::size_gb("disk", "Disk size")               // --disk <GB>
FieldDef::enum_field("algo", "Algorithm",
    &["round-robin", "least-conn"])                  // --algo round-robin|least-conn
FieldDef::resource_ref("vpc", "Target VPC", "vpc")  // --vpc <NAME_OR_ID>
```

Field modifiers:

```rust
FieldDef::string("desc", "Description")
    .mutable()               // can be patched after creation
    .with_default("none")    // default value
    .with_short('d')         // -d shorthand
    .with_env("SYFRAH_DESC") // read from env var
    .advanced()              // hidden from short help (-h)
```

Mutability controls when a field can be set:

| Mutability | create | update | CLI |
|-----------|--------|--------|-----|
| `CreateOnly` | yes | no | shown on create only |
| `Mutable` | yes | yes | shown on create and update |
| `ReadOnly` | no | no | never shown (computed) |
| `Internal` | no | no | never shown (internal) |

### Operations

Unified model for CRUD and custom actions. You never write CLI parsing code.

```rust
// Standard CRUD — one line each:
OperationDef::create()
OperationDef::list()
OperationDef::get()
OperationDef::delete()    // auto: --yes, confirmation prompt

// Custom actions:
OperationDef::action("drain", "Drain all connections")
    .with_confirm()        // adds --yes + prompt
    .with_arg(OperationArg::required("timeout", FieldDef::integer("timeout", "Drain timeout")))
    .with_example("syfrah lb drain my-lb --timeout 30")
    .with_output(OutputKind::Message)
    .with_success_message("{kind} '{name}' drained.")
```

What the framework handles automatically per operation type:

| Semantic | Positional `<NAME>` | `--json` | `--yes` | Confirmation | Scope flags |
|----------|:---:|:---:|:---:|:---:|:---:|
| Create | yes | - | - | - | required |
| List | - | yes | - | - | optional filters |
| Get | yes | yes | - | - | optional |
| Delete | yes | - | yes | yes | optional |
| Update | yes | - | - | - | optional |
| Action | - | - | if confirmable | if confirmable | - |

### Constraints

Cross-field validation. Checked before the handler is called.

```rust
// If protocol is "tcp", then --port is required
Constraint::Requires {
    if_field: "protocol",
    if_value: Some("tcp"),
    then_field: "port",
    message: "TCP requires --port",
}

// If protocol is "icmp", then --port must NOT be present
Constraint::Forbids {
    if_field: "protocol",
    if_value: Some("icmp"),
    then_field: "port",
    message: "ICMP does not use --port",
}

// --shared and --project cannot both be set
Constraint::Conflicts {
    a: "shared",
    b: "project",
    message: "Shared VPCs cannot belong to a project",
}

// Must specify exactly one of --ipv4 or --ipv6
Constraint::OneOf {
    fields: &["ipv4", "ipv6"],
    message: "Specify exactly one of --ipv4 or --ipv6",
}

// Arbitrary validation
Constraint::Custom {
    name: "cidr_range",
    validate: |fields| {
        if let Some(cidr) = fields.get("cidr") {
            if !cidr.contains('/') {
                return Err("CIDR must include prefix length (e.g. 10.0.0.0/16)".into());
            }
        }
        Ok(())
    },
}
```

### Presentation

Controls how resources are displayed. Two modes: table (for `list`) and detail (for `get`).

```rust
// Table columns
ColumnDef::new("NAME", "name")                          // plain text
ColumnDef::new("SIZE", "size").with_format(DisplayFormat::Bytes)      // 1073741824 → "1.0 GiB"
ColumnDef::new("UPTIME", "uptime").with_format(DisplayFormat::Duration) // 3661 → "1h 1m"
ColumnDef::new("CREATED", "created_at").with_format(DisplayFormat::Timestamp) // epoch → "2026-04-05 15:33 UTC"
ColumnDef::new("ACTIVE", "active").with_format(DisplayFormat::YesNo)  // true → "yes"
ColumnDef::new("SECRET", "key").with_format(DisplayFormat::Masked)    // "abc123" → "****...3123"
ColumnDef::new("COUNT", "count").fixed(8).right()        // fixed width, right-aligned
```

All display formats:

| Format | Input | Output |
|--------|-------|--------|
| `Plain` | `"hello"` | `hello` |
| `YesNo` | `true` | `yes` |
| `Bytes` | `1073741824` | `1.0 GiB` |
| `Duration` | `90061` | `1d 1h` |
| `Timestamp` | `1775403207` | `2026-04-05 15:33 UTC` |
| `Status` | `"Available"` | `Available` |
| `Masked` | `"syf_sk_abc123"` | `****...c123` |

## The builder API

For ergonomic resource definitions. Same result as constructing structs manually, but more readable.

```rust
let def = ResourceDef::build("sg", "Security Group")
    .plural("security-groups")
    .parent("vpc", "--vpc", "VPC the security group belongs to")
    .field(FieldDef::string("description", "SG description").mutable())
    .crud()
    .action("add-rule", "Add a firewall rule")
        .op(|op| op
            .with_arg(OperationArg::required("direction",
                FieldDef::enum_field("direction", "Traffic direction", &["ingress", "egress"])))
            .with_arg(OperationArg::required("protocol",
                FieldDef::enum_field("protocol", "Protocol", &["tcp", "udp", "icmp"])))
            .with_arg(OperationArg::optional("port", FieldDef::integer("port", "Port number")))
            .with_arg(OperationArg::required("source", FieldDef::cidr("source", "Source CIDR")))
            .with_constraint(Constraint::Requires {
                if_field: "protocol", if_value: Some("tcp"),
                then_field: "port", message: "TCP requires --port",
            })
            .with_constraint(Constraint::Forbids {
                if_field: "protocol", if_value: Some("icmp"),
                then_field: "port", message: "ICMP does not use --port",
            })
        )
    .action("attach", "Attach to a VM")
        .op(|op| op.with_arg(OperationArg::required("vm",
            FieldDef::resource_ref("vm", "Target VM", "vm"))))
    .column("NAME", "name")
    .column("VPC", "vpc")
    .column("RULES", "rule_count")
    .empty_message("No security groups found.")
    .done();
```

## The dispatch pipeline

When a user runs a command, the framework executes this pipeline:

```
1. Parse (clap)
   └─ CLI args → ArgMatches

2. Extract
   └─ ArgMatches → OperationRequest (name, scope, fields)

3. Validate
   └─ Check all constraints → fail fast with clear error
   └─ Produce ValidatedRequest

4. Confirm (if destructive)
   └─ "Delete vpc 'my-vpc'? [y/N]" → abort or continue

5. Handle
   └─ Call handler(request) → OperationResponse

6. Render
   └─ --json? → raw JSON
   └─ list? → table with DisplayFormats
   └─ get? → detail view with DisplayFormats
   └─ message? → success message with {kind}/{name} placeholders
```

## Guarantees

These are enforced by the framework, not by convention:

- Every `list` command has `--json`
- Every `get` command has `--json`
- Every `delete` command has `--yes`/`-y` and a confirmation prompt
- Every `create` command has a positional `<NAME>` argument
- Every resource with parents has scope flags (`--org`, `--vpc`, etc.)
- Table rendering is identical across all resources
- Error messages follow the same format
- Confirmation prompts follow the same format

Conformance tests verify these properties across all registered resources. If someone adds a resource that violates them, CI fails.

## Adding a new resource

1. Create a `fn my_resource() -> ResourceDef` using the builder
2. Write a handler function that processes `OperationRequest`
3. Register it: `registry.register(ResourceRegistration { def, handler })`
4. Done. The CLI, validation, rendering, and conformance tests are automatic.

Zero CLI code to write. Zero formatting code. Zero validation boilerplate.
