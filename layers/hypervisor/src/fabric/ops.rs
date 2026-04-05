//! Fabric operations — high-level orchestration.
//!
//! These are the public entry points that the hypervisor handler calls.
//! Each function orchestrates the lower-level modules (mesh, service, state, wg).

use syfrah_core::error::SyfrahError;
use syfrah_state::LayerDb;

use super::mesh::{self, HypervisorIdentity, MeshIdentity};
use super::peer::PeerList;
use super::service;
use super::state::FabricState;
use super::wg;

/// Result of a successful fabric init.
pub struct InitResult {
    pub mesh: MeshIdentity,
    pub hypervisor: HypervisorIdentity,
    pub secret_masked: String,
}

/// Initialize a new mesh on this node.
///
/// 1. Create mesh + hypervisor identity
/// 2. Install WireGuard systemd service
/// 3. Start the service
/// 4. Persist state
pub fn init(
    db: &LayerDb,
    mesh_name: &str,
    node_name: &str,
    region: &str,
    zone: &str,
    port: u16,
) -> Result<InitResult, SyfrahError> {
    // Check not already initialized
    if FabricState::exists(db).map_err(|e| SyfrahError::internal(e.to_string()))? {
        return Err(SyfrahError::conflict(
            "hypervisor",
            node_name,
            "already initialized. Run 'syfrah hypervisor leave' first.",
        ));
    }

    // Create identities
    let (mesh_id, secret) = mesh::create_mesh(mesh_name)?;
    let hv = mesh::create_hypervisor(node_name, region, zone, port, None, &mesh_id.prefix)?;

    // Install and start WireGuard service
    service::install(&hv.wg_private_key, port, &hv.mesh_ipv6, &[])?;
    service::enable_and_start()?;

    // Persist state
    let secret_str = secret.to_string();
    let state = FabricState {
        mesh: mesh_id.clone(),
        hypervisor: hv.clone(),
        secret: secret_str.clone(),
        peers: PeerList::new(),
    };
    state
        .save(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?;

    let secret_masked = format!(
        "{}...{}",
        &secret_str[..10],
        &secret_str[secret_str.len() - 4..]
    );

    Ok(InitResult {
        mesh: mesh_id,
        hypervisor: hv,
        secret_masked,
    })
}

/// Result of a successful fabric join.
pub struct JoinResult {
    pub mesh_name: String,
    pub hypervisor: HypervisorIdentity,
    pub peer_count: usize,
}

/// Join an existing cluster.
///
/// 1. TCP connect to target → peering exchange
/// 2. Receive mesh secret + peer list
/// 3. Create hypervisor identity from received mesh prefix
/// 4. Install WireGuard service with all peers
/// 5. Start the service
/// 6. Persist state
pub async fn join(
    db: &LayerDb,
    target: &str,
    node_name: &str,
    region: &str,
    zone: &str,
    port: u16,
    pin: Option<&str>,
) -> Result<JoinResult, SyfrahError> {
    // Check not already initialized
    if FabricState::exists(db).map_err(|e| SyfrahError::internal(e.to_string()))? {
        return Err(SyfrahError::conflict(
            "hypervisor",
            node_name,
            "already initialized. Run 'syfrah hypervisor leave' first.",
        ));
    }

    // Build join request
    // We need a temporary keypair to send in the request
    let (wg_private, wg_public) = syfrah_core::crypto::generate_wg_keypair();

    let request = super::peering::JoinRequest {
        name: node_name.to_string(),
        region: region.to_string(),
        zone: zone.to_string(),
        wg_public_key: wg_public.clone(),
        wg_port: port,
        endpoint: None, // will be discovered by the target
        pin: pin.map(|s| s.to_string()),
    };

    // TCP peering exchange
    let response = super::peering_client::join(target, request).await?;

    // Extract mesh info from response
    let mesh_name = response
        .mesh_name
        .ok_or_else(|| SyfrahError::internal("join response missing mesh_name"))?;
    let secret_str = response
        .secret
        .ok_or_else(|| SyfrahError::internal("join response missing secret"))?;
    let prefix = response
        .prefix
        .ok_or_else(|| SyfrahError::internal("join response missing prefix"))?;

    // Derive our mesh IPv6 from the prefix + our public key
    use base64::Engine as _;
    let pub_bytes = base64::engine::general_purpose::STANDARD
        .decode(&wg_public)
        .unwrap_or_default();
    let mesh_ipv6 = syfrah_core::addressing::derive_node_address(&prefix, &pub_bytes);

    // Validate our identity
    syfrah_core::validate::name(node_name)?;
    syfrah_core::validate::region(region)?;
    syfrah_core::validate::zone(zone)?;
    syfrah_core::validate::port(port)?;

    // Build hypervisor identity
    let hv = HypervisorIdentity {
        id: syfrah_core::id::HypervisorId::generate(),
        name: node_name.to_string(),
        region: region.to_string(),
        zone: zone.to_string(),
        wg_private_key: wg_private.clone(),
        wg_public_key: wg_public,
        wg_port: port,
        endpoint: None,
        mesh_ipv6,
    };

    // Build mesh identity
    let mesh_id = MeshIdentity {
        id: syfrah_core::id::MeshId::generate(),
        name: mesh_name.clone(),
        prefix,
    };

    // Build peer list from response (acceptor + existing peers)
    let mut peers = PeerList::new();
    if let Some(acceptor) = &response.acceptor {
        let _ = peers.add(super::peer::Peer::new(
            acceptor.name.clone(),
            acceptor.region.clone(),
            acceptor.zone.clone(),
            acceptor.wg_public_key.clone(),
            acceptor.endpoint.clone(),
            acceptor.mesh_ipv6,
        ));
    }
    for p in &response.peers {
        let _ = peers.add(super::peer::Peer::new(
            p.name.clone(),
            p.region.clone(),
            p.zone.clone(),
            p.wg_public_key.clone(),
            p.endpoint.clone(),
            p.mesh_ipv6,
        ));
    }

    let peer_count = peers.len();

    // Build WireGuard peer configs
    let peers_for_wg: Vec<_> = peers
        .peers
        .iter()
        .map(|p| {
            (
                p.wg_public_key.clone(),
                "25".to_string(),
                p.mesh_ipv6,
                p.endpoint.clone(),
            )
        })
        .collect();

    // Install and start WireGuard with all peers
    service::install(&wg_private, port, &mesh_ipv6, &peers_for_wg)?;
    service::enable_and_start()?;

    // Persist state
    let state = FabricState {
        mesh: mesh_id,
        hypervisor: hv.clone(),
        secret: secret_str,
        peers,
    };
    state
        .save(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?;

    Ok(JoinResult {
        mesh_name,
        hypervisor: hv,
        peer_count,
    })
}

/// Get the current fabric status.
pub struct StatusResult {
    pub hypervisor_name: String,
    pub hypervisor_id: String,
    pub mesh_name: String,
    pub region: String,
    pub zone: String,
    pub mesh_ipv6: String,
    pub state: String,
    pub service_installed: bool,
    pub service_active: bool,
    pub wg_interface_up: bool,
    pub peer_count: usize,
    pub active_peers: usize,
    pub wg_port: u16,
    pub wg_peer_count: usize,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub fn status(db: &LayerDb) -> Result<StatusResult, SyfrahError> {
    let state = FabricState::load(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?
        .ok_or_else(|| {
            SyfrahError::precondition("not initialized. Run 'syfrah hypervisor init' first.")
        })?;

    let svc_installed = service::is_installed();
    let svc_active = service::is_active();
    let wg_up = wg::interface_exists();

    let fabric_state = if svc_active && wg_up {
        "available"
    } else if svc_installed {
        "stopped"
    } else {
        "not installed"
    };

    let (wg_port, wg_peer_count, rx, tx) = if wg_up {
        match wg::get_status() {
            Ok(s) => (s.listen_port, s.peer_count, s.rx_bytes, s.tx_bytes),
            Err(_) => (0, 0, 0, 0),
        }
    } else {
        (0, 0, 0, 0)
    };

    Ok(StatusResult {
        hypervisor_name: state.hypervisor.name,
        hypervisor_id: state.hypervisor.id.to_string(),
        mesh_name: state.mesh.name,
        region: state.hypervisor.region,
        zone: state.hypervisor.zone,
        mesh_ipv6: state.hypervisor.mesh_ipv6.to_string(),
        state: fabric_state.to_string(),
        service_installed: svc_installed,
        service_active: svc_active,
        wg_interface_up: wg_up,
        peer_count: state.peers.len(),
        active_peers: state.peers.active_count(),
        wg_port,
        wg_peer_count,
        rx_bytes: rx,
        tx_bytes: tx,
    })
}

/// Start the WireGuard service.
pub fn start() -> Result<(), SyfrahError> {
    if !service::is_installed() {
        return Err(SyfrahError::precondition(
            "not initialized. Run 'syfrah hypervisor init' first.",
        ));
    }
    if service::is_active() {
        return Ok(()); // already running, idempotent
    }
    service::start()
}

/// Stop the WireGuard service.
pub fn stop() -> Result<(), SyfrahError> {
    if !service::is_active() {
        return Ok(()); // already stopped, idempotent
    }
    service::stop()
}

/// Leave the cluster — uninstall service, remove state.
pub fn leave(db: &LayerDb) -> Result<(), SyfrahError> {
    // Uninstall systemd service + config
    service::uninstall()?;

    // Cleanup interface if still up
    if wg::interface_exists() {
        let _ = wg::destroy_interface();
    }

    // Delete state
    FabricState::delete(db).map_err(|e| SyfrahError::internal(e.to_string()))?;

    Ok(())
}
