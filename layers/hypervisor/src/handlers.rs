//! Hypervisor resource definition + handlers.
//!
//! No daemon. Each command configures system services and exits.
//! WireGuard runs in the kernel, not in a process.

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
        // Lifecycle
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
        // Service management (orchestrates systemd, not a daemon)
        .action("start", "Start the WireGuard service")
        .action("stop", "Stop the WireGuard service")
        .action("leave", "Leave the cluster, uninstall WireGuard service")
            .op(|op| op.with_confirm())
        .action("drain", "Evacuate all VMs before maintenance")
            .op(|op| op.with_confirm())
        .action("enable", "Enable for VM scheduling")
        // Table
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
        ])
        .done()
}

/// Build the handler.
pub fn handler() -> HandlerFn {
    Box::new(|req: OperationRequest| -> Pin<Box<dyn Future<Output = anyhow::Result<OperationResponse>> + Send>> {
        Box::pin(async move {
            match req.operation.as_str() {
                "init" => handle_init(req).await,
                "status" => handle_status(req).await,
                "list" => handle_list(req).await,
                "get" => handle_get(req).await,
                "leave" => handle_leave(req).await,
                // Stubs
                "start" => handle_start(req).await,
                "stop" => handle_stop(req).await,
                "join" => Ok(OperationResponse::Message("join not yet implemented — needs peering TCP server on target".into())),
                "drain" => Ok(OperationResponse::Message("drain not yet implemented".into())),
                "enable" => Ok(OperationResponse::Message("enable not yet implemented".into())),
                other => Ok(OperationResponse::Message(format!("unknown operation: {other}"))),
            }
        })
    })
}

/// Build the ResourceRegistration.
pub fn registration() -> ResourceRegistration {
    ResourceRegistration {
        def: resource_def(),
        handler: handler(),
    }
}

// ═══════════════════════════════════════════════════
// Handlers
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

    let node_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.to_lowercase())
        .unwrap_or_else(|| "node".to_string());

    // Check not already initialized
    let db = open_db()?;
    if fabric::state::FabricState::exists(&db).map_err(|e| anyhow::anyhow!("{e}"))? {
        anyhow::bail!("already initialized. Run 'syfrah hypervisor leave' first.");
    }

    // Create mesh + hypervisor identity
    let (mesh, secret) = fabric::mesh::create_mesh(mesh_name)?;
    let hv = fabric::mesh::create_hypervisor(&node_name, region, zone, port, None, &mesh.prefix)?;

    // Install and start WireGuard as a systemd service
    fabric::service::install(&hv.wg_private_key, port, &hv.mesh_ipv6, &[])?;
    fabric::service::enable_and_start()?;

    // Persist state
    let state = fabric::state::FabricState {
        mesh: mesh.clone(),
        hypervisor: hv.clone(),
        secret: secret.to_string(),
        peers: fabric::peer::PeerList::new(),
    };
    state.save(&db).map_err(|e| anyhow::anyhow!("{e}"))?;

    let secret_str = secret.to_string();
    let secret_masked = format!(
        "{}...{}",
        &secret_str[..10],
        &secret_str[secret_str.len() - 4..]
    );

    Ok(OperationResponse::Resource(serde_json::json!({
        "name": hv.name,
        "id": hv.id.as_str(),
        "mesh": mesh.name,
        "region": format!("{} · {}", region, zone),
        "zone": zone,
        "mesh_ipv6": hv.mesh_ipv6.to_string(),
        "wg_port": port,
        "state": "available",
        "secret": secret_masked,
    })))
}

async fn handle_status(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let db = open_db()?;
    let state = fabric::state::FabricState::load(&db)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!("not initialized. Run 'syfrah hypervisor init' first."))?;

    // Read live service + WireGuard status
    let svc_active = fabric::service::is_active();
    let svc_installed = fabric::service::is_installed();
    let wg_up = fabric::wg::interface_exists();
    let wg_info = if wg_up {
        fabric::wg::get_status().ok()
    } else {
        None
    };

    let interface_state = if svc_active && wg_up {
        "available"
    } else if svc_installed {
        "stopped"
    } else {
        "not installed"
    };

    let mut info = serde_json::json!({
        "name": state.hypervisor.name,
        "id": state.hypervisor.id.as_str(),
        "mesh": state.mesh.name,
        "region": state.hypervisor.region,
        "zone": state.hypervisor.zone,
        "mesh_ipv6": state.hypervisor.mesh_ipv6.to_string(),
        "state": interface_state,
        "peers": state.peers.len(),
        "active_peers": state.peers.active_count(),
        "wg_interface": wg_up,
    });

    if let Some(wg) = wg_info {
        info["wg_port"] = serde_json::json!(wg.listen_port);
        info["wg_peers"] = serde_json::json!(wg.peer_count);
        info["rx_bytes"] = serde_json::json!(wg.rx_bytes);
        info["tx_bytes"] = serde_json::json!(wg.tx_bytes);
    }

    Ok(OperationResponse::Resource(info))
}

async fn handle_list(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let db = open_db()?;
    let state = match fabric::state::FabricState::load(&db).map_err(|e| anyhow::anyhow!("{e}"))? {
        Some(s) => s,
        None => return Ok(OperationResponse::ResourceList(vec![])),
    };

    let wg_up = fabric::wg::interface_exists();
    let self_state = if wg_up { "available" } else { "down" };

    let mut items = vec![serde_json::json!({
        "name": state.hypervisor.name,
        "region": format!("{}/{}", state.hypervisor.region, state.hypervisor.zone),
        "state": self_state,
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

    if state.hypervisor.name == name {
        let wg_up = fabric::wg::interface_exists();
        return Ok(OperationResponse::Resource(serde_json::json!({
            "name": state.hypervisor.name,
            "id": state.hypervisor.id.as_str(),
            "region": state.hypervisor.region,
            "zone": state.hypervisor.zone,
            "mesh_ipv6": state.hypervisor.mesh_ipv6.to_string(),
            "state": if wg_up { "available" } else { "down" },
            "wg_port": state.hypervisor.wg_port,
        })));
    }

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

async fn handle_start(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    if !fabric::service::is_installed() {
        anyhow::bail!("not initialized. Run 'syfrah hypervisor init' first.");
    }
    if fabric::service::is_active() {
        return Ok(OperationResponse::Message(
            "WireGuard service already running.".into(),
        ));
    }
    fabric::service::start()?;
    Ok(OperationResponse::Message(
        "WireGuard service started.".into(),
    ))
}

async fn handle_stop(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    if !fabric::service::is_active() {
        return Ok(OperationResponse::Message(
            "WireGuard service already stopped.".into(),
        ));
    }
    fabric::service::stop()?;
    Ok(OperationResponse::Message(
        "WireGuard service stopped.".into(),
    ))
}

async fn handle_leave(_req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let db = open_db()?;

    // Uninstall systemd service + remove WireGuard config
    fabric::service::uninstall()?;

    // Also remove interface if wg-quick didn't
    if fabric::wg::interface_exists() {
        let _ = fabric::wg::destroy_interface();
    }

    // Delete state
    fabric::state::FabricState::delete(&db).map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(OperationResponse::Message(
        "left the cluster. WireGuard service uninstalled.".into(),
    ))
}

fn open_db() -> anyhow::Result<LayerDb> {
    let dir = syfrah_core::process::syfrah_dir();
    std::fs::create_dir_all(&dir)?;
    LayerDb::open("hypervisor").map_err(|e| anyhow::anyhow!("{e}"))
}
