//! Hypervisor resource definition + handlers.
//!
//! Handlers delegate to fabric::ops. No plumbing here — just
//! translate OperationRequest → fabric call → OperationResponse.

use std::future::Future;
use std::pin::Pin;

use syfrah_core::resource::*;
use syfrah_state::LayerDb;

use crate::controlplane;
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
                .with_arg(OperationArg::optional("region", FieldDef::string("region", "Region label").with_default("default")))
                .with_arg(OperationArg::optional("zone", FieldDef::string("zone", "Zone label").with_default("default")))
                .with_arg(OperationArg::optional("port", FieldDef::integer("port", "WireGuard listen port").with_default("51820")))
                .with_arg(OperationArg::optional("peering", FieldDef::flag("peering", "Start peering listener after init (accepts joins)")))
                .with_output(OutputKind::Resource)
                .with_example("syfrah hypervisor init --name my-cloud --region eu --zone fsn1 --peering")
            )
        .action("join", "Join an existing cluster")
            .op(|op| op
                .with_arg(OperationArg::required("target", FieldDef::string("target", "IP or IP:port of an existing node")))
                .with_arg(OperationArg::optional("pin", FieldDef::string("pin", "PIN for auto-accept")))
                .with_arg(OperationArg::optional("region", FieldDef::string("region", "Region label").with_default("default")))
                .with_arg(OperationArg::optional("zone", FieldDef::string("zone", "Zone label").with_default("default")))
                .with_arg(OperationArg::optional("port", FieldDef::integer("port", "WireGuard listen port").with_default("51820")))
                .with_output(OutputKind::Resource)
                .with_example("syfrah hypervisor join --target 46.224.166.60 --pin G7CCZX --region eu --zone nbg1")
            )
        .action("status", "Show hypervisor status")
            .op(|op| op.with_output(OutputKind::Resource))
        .action("start", "Start the WireGuard service")
        .action("stop", "Stop the WireGuard service")
        .action("leave", "Leave the cluster, uninstall WireGuard service")
            .op(|op| op.with_confirm())
        // CRUD
        .list().op(|op| op.with_example("syfrah hypervisor list"))
        .get().op(|op| op.with_example("syfrah hypervisor get HYPERVISOR-1"))
        .action("peering", "Start peering listener to accept new nodes")
            .op(|op| op
                .with_arg(OperationArg::optional("timeout", FieldDef::integer("timeout", "Listener timeout in seconds").with_default("3600")))
                .with_example("syfrah hypervisor peering")
            )
        // Future
        .action("drain", "Evacuate all VMs before maintenance").op(|op| op.with_confirm())
        .action("enable", "Enable for VM scheduling")
        // Table
        .column("NAME", "name")
        .column("REGION", "region")
        .column_def(ColumnDef::new("STATE", "state").with_format(DisplayFormat::Status))
        .column("CPU", "cpu")
        .column("MEMORY", "memory")
        .column("VMs", "vms")
        .empty_message("No hypervisors found. Initialize with: syfrah hypervisor init --name <mesh>")
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

pub fn handler() -> HandlerFn {
    Box::new(|req: OperationRequest| -> Pin<Box<dyn Future<Output = anyhow::Result<OperationResponse>> + Send>> {
        Box::pin(async move {
            match req.operation.as_str() {
                "init" => handle_init(req).await,
                "status" => handle_status().await,
                "start" => handle_start().await,
                "stop" => handle_stop().await,
                "leave" => handle_leave().await,
                "list" => handle_list().await,
                "get" => handle_get(req).await,
                "join" => handle_join(req).await,
                "peering" => handle_peering(req).await,
                "drain" => Ok(OperationResponse::Message("drain: not yet implemented".into())),
                "enable" => Ok(OperationResponse::Message("enable: not yet implemented".into())),
                other => Ok(OperationResponse::Message(format!("unknown: {other}"))),
            }
        })
    })
}

pub fn registration() -> ResourceRegistration {
    ResourceRegistration {
        def: resource_def(),
        handler: handler(),
    }
}

// ═══════════════════════════════════════════════════
// Thin handlers — delegate to fabric::ops
// ═══════════════════════════════════════════════════

async fn handle_init(req: OperationRequest) -> anyhow::Result<OperationResponse> {
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

    let peering = req
        .fields
        .get("peering")
        .map(|s| s == "true")
        .unwrap_or(false);

    let db = open_db()?;
    let result = fabric::ops::init(&db, &node_name, region, zone, port)?;

    // Bootstrap control plane (TiKV) on the mesh — rollback fabric on failure
    if let Err(e) = controlplane::ops::bootstrap(&node_name, &result.hypervisor.mesh_ipv6) {
        tracing::error!(error = %e, "control plane bootstrap failed, rolling back fabric");
        let _ = fabric::ops::leave(&db);
        return Err(anyhow::anyhow!("control plane failed: {e}. Fabric rolled back."));
    }

    eprintln!();
    eprintln!("  Hypervisor initialized");
    eprintln!();
    eprintln!("  name     {}", result.hypervisor.name);
    eprintln!("  id       {}", result.hypervisor.id.as_str());
    eprintln!("  mesh     {}", result.mesh.id);
    eprintln!("  region   {} · {}", region, zone);
    eprintln!("  address  {}", result.hypervisor.mesh_ipv6);
    eprintln!("  pin      {}", result.pin);
    eprintln!();

    if peering {
        let peering_port = port + 1;
        eprintln!("  Peering active on port {peering_port}");
        eprintln!("  Nodes can join with:");
        eprintln!(
            "    syfrah hypervisor join --target <this-ip>:{peering_port} --pin {}",
            result.pin
        );
        eprintln!();
        eprintln!("  Waiting for joins... (Ctrl+C to stop)");
        eprintln!();

        // Block here listening for joins (DB opened per-request, no lock held)
        let accepted = fabric::ops::listen_for_peers(
            &result.pin,
            peering_port,
            3600, // 1 hour timeout
        )
        .await?;

        eprintln!("  {} node(s) joined.", accepted);
    } else {
        eprintln!("  To accept joins, run:");
        eprintln!("  Or on another node:");
        eprintln!(
            "    syfrah hypervisor join --target <this-ip> --pin {}",
            result.pin
        );
    }

    Ok(OperationResponse::Resource(serde_json::json!({
        "name": result.hypervisor.name,
        "id": result.hypervisor.id.as_str(),
        "mesh_id": result.hypervisor.id.as_str(),
        "region": format!("{} · {}", region, zone),
        "zone": zone,
        "mesh_ipv6": result.hypervisor.mesh_ipv6.to_string(),
        "state": "available",
        "pin": result.pin,
    })))
}

async fn handle_join(req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let target = req
        .fields
        .get("target")
        .ok_or_else(|| anyhow::anyhow!("missing required field: target"))?
        .clone();
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
    let pin = req.fields.get("pin").map(|s| s.as_str());

    let node_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.to_lowercase())
        .unwrap_or_else(|| "node".to_string());

    let db = open_db()?;
    let result = fabric::ops::join(&db, &target, &node_name, region, zone, port, pin).await?;

    // Join control plane — build PD endpoints from peers' mesh addresses
    let state = fabric::state::FabricState::load(&db)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!("state missing after join"))?;
    let pd_endpoints: Vec<String> = state
        .peers
        .peers
        .iter()
        .map(|p| format!("http://[{}]:{}", p.mesh_ipv6, controlplane::PD_CLIENT_PORT))
        .collect();
    if let Err(e) = controlplane::ops::join(&node_name, &result.hypervisor.mesh_ipv6, &pd_endpoints) {
        tracing::error!(error = %e, "control plane join failed, rolling back fabric");
        let _ = fabric::ops::leave(&db);
        return Err(anyhow::anyhow!("control plane failed: {e}. Fabric rolled back."));
    }

    Ok(OperationResponse::Resource(serde_json::json!({
        "name": result.hypervisor.name,
        "id": result.hypervisor.id.as_str(),
        "mesh_id": result.hypervisor.id.as_str(),
        "region": format!("{} · {}", region, zone),
        "zone": zone,
        "mesh_ipv6": result.hypervisor.mesh_ipv6.to_string(),
        "state": "available",
        "peers": result.peer_count,
    })))
}

async fn handle_peering(req: OperationRequest) -> anyhow::Result<OperationResponse> {
    let timeout_secs: u64 = req
        .fields
        .get("timeout")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);

    let db = open_db()?;
    let state = fabric::state::FabricState::load(&db)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!("not initialized. Run 'syfrah hypervisor init' first."))?;

    // Derive PIN from secret
    let secret: syfrah_core::crypto::MeshSecret = state.secret.parse()?;
    let pin = secret.derive_pin();
    let peering_port = state.hypervisor.wg_port + 1;

    // Drop DB before starting listener (release lock!)
    drop(db);

    eprintln!();
    eprintln!("  Peering active on port {peering_port}");
    eprintln!("  PIN: {pin}");
    eprintln!();
    eprintln!("  Nodes can join with:");
    eprintln!("    syfrah hypervisor join --target <this-ip>:{peering_port} --pin {pin}");
    eprintln!();
    eprintln!("  Waiting for joins... (Ctrl+C to stop)");
    eprintln!();

    let accepted = fabric::ops::listen_for_peers(&pin, peering_port, timeout_secs).await?;

    Ok(OperationResponse::Message(format!(
        "{accepted} node(s) joined."
    )))
}

async fn handle_status() -> anyhow::Result<OperationResponse> {
    let db = open_db()?;
    let s = fabric::ops::status(&db)?;

    // Control plane status (best-effort — may not be installed yet)
    let cp = controlplane::ops::status(&s.mesh_ipv6.parse().unwrap_or(std::net::Ipv6Addr::UNSPECIFIED));
    let (pd_active, tikv_active, pd_members, tikv_stores, leader) = match cp {
        Ok(cs) => (cs.pd_active, cs.tikv_active, cs.pd_members, cs.tikv_stores, cs.leader),
        Err(_) => (false, false, 0, 0, None),
    };

    Ok(OperationResponse::Resource(serde_json::json!({
        "name": s.hypervisor_name,
        "id": s.hypervisor_id,
        "mesh_id": s.hypervisor_id,
        "region": s.region,
        "zone": s.zone,
        "mesh_ipv6": s.mesh_ipv6,
        "state": s.state,
        "wg": if s.service_active { "running" } else { "stopped" },
        "wg_interface": s.wg_interface_up,
        "peers": s.peer_count,
        "wg_port": s.wg_port,
        "rx_bytes": s.rx_bytes,
        "tx_bytes": s.tx_bytes,
        "pd": if pd_active { "running" } else { "stopped" },
        "tikv": if tikv_active { "running" } else { "stopped" },
        "pd_members": pd_members,
        "tikv_stores": tikv_stores,
        "leader": leader,
    })))
}

async fn handle_start() -> anyhow::Result<OperationResponse> {
    fabric::ops::start()?;
    let _ = controlplane::ops::start();
    Ok(OperationResponse::Message("services started.".into()))
}

async fn handle_stop() -> anyhow::Result<OperationResponse> {
    let _ = controlplane::ops::stop();
    fabric::ops::stop()?;
    Ok(OperationResponse::Message("services stopped.".into()))
}

async fn handle_leave() -> anyhow::Result<OperationResponse> {
    let db = open_db()?;
    // Get mesh IPv6 for TiKV deregistration before leaving
    let mesh_ipv6 = fabric::state::FabricState::load(&db)
        .ok()
        .flatten()
        .map(|s| s.hypervisor.mesh_ipv6);

    // Controlplane first (deregister store, then uninstall)
    if let Some(ipv6) = mesh_ipv6 {
        let _ = controlplane::ops::leave_with_mesh(&ipv6);
    } else {
        let _ = controlplane::ops::leave();
    }

    // Then fabric
    fabric::ops::leave(&db)?;
    Ok(OperationResponse::Message(
        "left the cluster. All services uninstalled.".into(),
    ))
}

async fn handle_list() -> anyhow::Result<OperationResponse> {
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
        return Ok(OperationResponse::Resource(serde_json::json!({
            "name": state.hypervisor.name,
            "id": state.hypervisor.id.as_str(),
            "region": state.hypervisor.region,
            "zone": state.hypervisor.zone,
            "mesh_ipv6": state.hypervisor.mesh_ipv6.to_string(),
            "state": if fabric::wg::interface_exists() { "available" } else { "down" },
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

fn open_db() -> anyhow::Result<LayerDb> {
    let dir = syfrah_core::process::syfrah_dir();
    std::fs::create_dir_all(&dir)?;
    LayerDb::open("hypervisor").map_err(|e| anyhow::anyhow!("{e}"))
}
