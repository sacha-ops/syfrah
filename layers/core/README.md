# syfrah-core

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
