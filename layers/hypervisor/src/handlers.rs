//! Hypervisor resource definition + handlers.
//!
//! Defines the ResourceDef that generates both CLI and API routes,
//! and the handler that processes operations.

use std::future::Future;
use std::pin::Pin;

use syfrah_core::resource::*;
use syfrah_state::LayerDb;

use crate::fabric;

/// Build the hypervisor ResourceDef.
pub fn resource_def() -> ResourceDef {
    ResourceDef::build("hypervisor", "Manage hypervisors (compute hosts)")
        .alias("hv")
        .alias("node")
        .plural("hypervisors")
        // Lifecycle actions
        .action("init", "Initialize a new cluster")
            .op(|op| op
                .with_arg(OperationArg::required("name", FieldDef::string("name", "Mesh name")))
                .with_arg(OperationArg::optional("region", FieldDef::string("region", "Region label").with_default("default")))
                .with_arg(OperationArg::optional("zone", FieldDef::string("zone", "Zone label").with_default("default")))
                .with_arg(OperationArg::optional("port", FieldDef::integer("port", "WireGuard listen port").with_default("51820")))
                .with_output(OutputKind::Resource)
                .with_example("syfrah hypervisor init --name my-cloud --region eu --zone fsn1")
            )
        .action("join", "Join an existing cluster")
            .op(|op| op
                .with_arg(OperationArg::required("target", FieldDef::string("target", "IP or IP:port of an existing node")))
                .with_arg(OperationArg::optional("pin", FieldDef::string("pin", "PIN for auto-accept")))
                .with_arg(OperationArg::optional("name", FieldDef::string("name", "Node name (default: hostname)")))
                .with_arg(OperationArg::optional("region", FieldDef::string("region", "Region label").with_default("default")))
                .with_arg(OperationArg::optional("zone", FieldDef::string("zone", "Zone label").with_default("default")))
                .with_arg(OperationArg::optional("port", FieldDef::integer("port", "WireGuard listen port").with_default("51820")))
                .with_output(OutputKind::Resource)
                .with_example("syfrah hypervisor join --target 46.224.166.60 --pin G7CCZX --region eu --zone nbg1")
            )
        .action("status", "Show hypervisor status")
            .op(|op| op.with_output(OutputKind::Resource))
        // CRUD
        .list()
            .op(|op| op.with_example("syfrah hypervisor list"))
        .get()
            .op(|op| op.with_example("syfrah hypervisor get HYPERVISOR-1"))
        // Operations
        .action("start", "Start the daemon from saved state")
        .action("stop", "Stop the running daemon")
            .op(|op| op.with_confirm())
        .action("leave", "Leave the cluster and tear down")
            .op(|op| op.with_confirm())
        .action("drain", "Evacuate all VMs before maintenance")
            .op(|op| op.with_confirm())
        .action("enable", "Enable for VM scheduling")
        // Table presentation
        .column("NAME", "name")
        .column("REGION", "region")
        .column_def(ColumnDef::new("STATE", "state").with_format(DisplayFormat::Status))
        .column("CPU", "cpu")
        .column("MEMORY", "memory")
        .column("VMs", "vms")
        .empty_message("No hypervisors found. Initialize with: syfrah hypervisor init --name <mesh>")
        // Detail
        .detail_section(None, vec![
            DetailField::new("Name", "name"),
            DetailField::new("ID", "id"),
            DetailField::new("Region", "region"),
            DetailField::new("Zone", "zone"),
            DetailField::new("Address", "mesh_ipv6"),
            DetailField::new("State", "state").with_format(DisplayFormat::Status),
            DetailField::new("Uptime", "uptime").with_format(DisplayFormat::Duration),
        ])
        .done()
}

/// Build the handler function for the hypervisor resource.
pub fn handler() -> HandlerFn {
    Box::new(|req: OperationRequest| -> Pin<Box<dyn Future<Output = anyhow::Result<OperationResponse>> + Send>> {
        Box::pin(async move {
            match req.operation.as_str() {
                "init" => handle_init(req).await,
                "status" => handle_status(req).await,
                "list" => handle_list(req).await,
                "get" => handle_get(req).await,
                // Stubs for now — will be implemented with WireGuard/peering
                "join" => Ok(OperationResponse::Message("join not yet implemented".into())),
                "start" => Ok(OperationResponse::Message("start not yet implemented".into())),
                "stop" => Ok(OperationResponse::Message("stop not yet implemented".into())),
                "leave" => Ok(OperationResponse::Message("leave not yet implemented".into())),
                "drain" => Ok(OperationResponse::Message("drain not yet implemented".into())),
                "enable" => Ok(OperationResponse::Message("enable not yet implemented".into())),
                other => Ok(OperationResponse::Message(format!("unknown operation: {other}"))),
            }
        })
    })
}

/// Build the ResourceRegistration (def + handler together).
pub fn registration() -> ResourceRegistration {
    ResourceRegistration {
        def: resource_def(),
        handler: handler(),
    }
}

// ═══════════════════════════════════════════════════
// Handler implementations
// ═══════════════════════════════════════════════════

async fn handle_init(req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let mesh_name = req
        .fields
        .get("name")
        .ok_or_else(|| anyhow::anyhow!("missing required field: name"))?;
    let region = req
        .fields
        .get("region")
        .map(|s| s.as_str())
        .unwrap_or("default");
    let zone = req
        .fields
        .get("zone")
        .map(|s| s.as_str())
        .unwrap_or("default");
    let port: u16 = req
        .fields
        .get("port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(51820);

    // Get hostname as default node name
    let node_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.to_lowercase())
        .unwrap_or_else(|| "node".to_string());

    // Create mesh + node identity
    let (mesh, secret) = fabric::mesh::create_mesh(mesh_name)?;
    let node = fabric::mesh::create_node(&node_name, region, zone, port, None, &mesh.prefix)?;

    // Persist state
    let db = open_db()?;
    if fabric::state::FabricState::exists(&db).map_err(|e| anyhow::anyhow!("{e}"))? {
        anyhow::bail!(
            "mesh already initialized on this node. Run 'syfrah hypervisor leave' first."
        );
    }

    let state = fabric::state::FabricState {
        mesh: mesh.clone(),
        node: node.clone(),
        secret: secret.to_string(),
        peers: fabric::peer::PeerList::new(),
    };
    state.save(&db).map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(OperationResponse::Resource(serde_json::json!({
        "name": node.name,
        "id": node.id.as_str(),
        "mesh": mesh.name,
        "mesh_id": mesh.id.as_str(),
        "region": format!("{} · {}", region, zone),
        "zone": zone,
        "mesh_ipv6": node.mesh_ipv6.to_string(),
        "wg_port": port,
        "state": "initialized",
        "secret": format!("{}...{}", &secret.to_string()[..10], &secret.to_string()[secret.to_string().len()-4..]),
    })))
}

async fn handle_status(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let db = open_db()?;
    let state = fabric::state::FabricState::load(&db)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!("not initialized. Run 'syfrah hypervisor init' first."))?;

    Ok(OperationResponse::Resource(serde_json::json!({
        "name": state.node.name,
        "id": state.node.id.as_str(),
        "mesh": state.mesh.name,
        "region": state.node.region,
        "zone": state.node.zone,
        "mesh_ipv6": state.node.mesh_ipv6.to_string(),
        "state": "initialized",
        "peers": state.peers.len(),
        "active_peers": state.peers.active_count(),
    })))
}

async fn handle_list(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let db = open_db()?;
    let state = match fabric::state::FabricState::load(&db).map_err(|e| anyhow::anyhow!("{e}"))? {
        Some(s) => s,
        None => return Ok(OperationResponse::ResourceList(vec![])),
    };

    // For now, just list self + peers as "hypervisors"
    let mut items = vec![serde_json::json!({
        "name": state.node.name,
        "region": format!("{}/{}", state.node.region, state.node.zone),
        "state": "available",
        "cpu": "0/0",
        "memory": "0/0",
        "vms": 0,
    })];

    for peer in &state.peers.peers {
        items.push(serde_json::json!({
            "name": peer.name,
            "region": format!("{}/{}", peer.region, peer.zone),
            "state": format!("{:?}", peer.status).to_lowercase(),
            "cpu": "0/0",
            "memory": "0/0",
            "vms": 0,
        }));
    }

    Ok(OperationResponse::ResourceList(items))
}

async fn handle_get(req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let name = req
        .name
        .ok_or_else(|| anyhow::anyhow!("missing hypervisor name"))?;

    let db = open_db()?;
    let state = fabric::state::FabricState::load(&db)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!("not initialized"))?;

    // Check if it's this node
    if state.node.name == name {
        return Ok(OperationResponse::Resource(serde_json::json!({
            "name": state.node.name,
            "id": state.node.id.as_str(),
            "region": state.node.region,
            "zone": state.node.zone,
            "mesh_ipv6": state.node.mesh_ipv6.to_string(),
            "state": "available",
            "wg_port": state.node.wg_port,
        })));
    }

    // Check peers
    if let Some(peer) = state.peers.find_by_name(&name) {
        return Ok(OperationResponse::Resource(serde_json::json!({
            "name": peer.name,
            "region": peer.region,
            "zone": peer.zone,
            "mesh_ipv6": peer.mesh_ipv6.to_string(),
            "state": format!("{:?}", peer.status).to_lowercase(),
        })));
    }

    anyhow::bail!("hypervisor '{name}' not found")
}

fn open_db() -> anyhow::Result<LayerDb> {
    syfrah_core::process::ensure_syfrah_dir().map_err(|e| anyhow::anyhow!("{e}"))?;
    LayerDb::open("hypervisor").map_err(|e| anyhow::anyhow!("{e}"))
}
