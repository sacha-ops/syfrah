use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use wireguard_control::{Key, KeyPair};

use syfrah_core::addressing;
use syfrah_core::mesh::{PeerRecord, PeerStatus};
use syfrah_core::secret::MeshSecret;

use crate::audit::{self as audit_log, AuditEventType};
use crate::config::{self, Tuning};
use crate::control::{self, FabricHandler, FabricLayerHandler, FabricRequest, FabricResponse};
use crate::events::{self, EventType};
use crate::http_api;
use crate::peering::{self, AutoAcceptConfig, PeeringState};
use crate::sanitize::sanitize;
use crate::sd_watchdog;
use crate::store::{self, NodeState};
use crate::ui;
use crate::wg;
use syfrah_api::LayerRouter;

/// TLS certificate verifier that skips trust-anchor verification but still
/// validates TLS 1.3 handshake signatures.  Used only during the join
/// handshake where the joiner does not yet have the mesh secret to verify
/// the server's certificate chain.  The PIN exchange provides the
/// authentication guarantee at this stage.
mod danger {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::DigitallySignedStruct;

    #[derive(Debug)]
    pub struct NoCertVerifier;

    impl ServerCertVerifier for NoCertVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            // Skip trust-anchor check — the joiner cannot verify the
            // mesh-derived CA yet.  Signature math is still enforced below.
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            // TLS 1.2 is disabled at the config level; reject if reached.
            Err(rustls::Error::General(
                "TLS 1.2 is not supported".to_string(),
            ))
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            // Delegate to the real ring signature verifier so that a MITM
            // cannot present an arbitrary certificate with a garbage signature.
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}

/// Default region used when the operator does not specify `--region`.
pub const DEFAULT_REGION: &str = "default";

/// Resolve region and zone from optional user-provided values.
///
/// - If `region` is `None`, falls back to [`DEFAULT_REGION`].
/// - If `zone` is `None`, auto-generates one via [`store::generate_zone`].
///
/// This is the single source of truth used by `setup_init`, `setup_join`,
/// `auto_init`, and the join-accept handler.
pub fn resolve_region_zone(
    region: Option<&str>,
    zone: Option<&str>,
    existing_peers: &[syfrah_core::mesh::PeerRecord],
) -> (String, String) {
    let region = region
        .map(|r| r.to_string())
        .unwrap_or_else(|| DEFAULT_REGION.to_string());
    let zone = zone
        .map(|z| z.to_string())
        .unwrap_or_else(|| store::generate_zone(&region, existing_peers));
    (region, zone)
}

pub struct DaemonConfig {
    pub mesh_name: String,
    pub node_name: String,
    pub wg_listen_port: u16,
    pub public_endpoint: Option<SocketAddr>,
    pub peering_port: u16,
    pub region: Option<String>,
    pub zone: Option<String>,
}

/// Data produced by `setup_init` / `setup_join`, needed to start the daemon loop.
pub struct DaemonReady {
    pub my_record: PeerRecord,
    pub wg_keypair: KeyPair,
    pub mesh_secret: MeshSecret,
    pub peering_port: u16,
}

/// Setup the init flow: create a new mesh, save state, print info.
/// Returns a DaemonReady that can be passed to run_daemon.
pub fn setup_init(config: &DaemonConfig) -> anyhow::Result<DaemonReady> {
    if store::exists() {
        anyhow::bail!("mesh state already exists. Run 'syfrah fabric leave' first.");
    }

    let tuning = config::load_tuning().unwrap_or_default();
    wg::set_interface_name(&tuning.interface_name);

    let sp = ui::spinner("Generating mesh secret...");
    let mesh_secret = MeshSecret::generate();
    let wg_keypair = wg::generate_keypair();
    let mesh_prefix = derive_prefix_from_secret(&mesh_secret);
    let mesh_ipv6 = addressing::derive_node_address(&mesh_prefix, wg_keypair.public.as_bytes());
    let endpoint = resolve_endpoint(config);
    ui::step_ok(&sp, "Mesh secret generated (stored in state file)");

    let sp = ui::spinner("Setting up WireGuard interface...");
    wg::setup_interface(&wg_keypair, config.wg_listen_port, mesh_ipv6)?;
    ui::step_ok(
        &sp,
        &format!("Interface {} up ({mesh_ipv6})", wg::interface_name()),
    );
    info!(flow = "init", mesh = %config.mesh_name, node = %config.node_name, "wireguard interface up");

    // Region/zone: use provided or defaults
    let (region, zone) = resolve_region_zone(config.region.as_deref(), config.zone.as_deref(), &[]);
    if config.region.is_none() {
        ui::warn("No --region specified; using 'default'. Set --region to label this node.");
    }

    let state = NodeState {
        mesh_name: config.mesh_name.clone(),
        mesh_secret: mesh_secret.to_string(),
        wg_private_key: wg_keypair.private.to_base64(),
        wg_public_key: wg_keypair.public.to_base64(),
        mesh_ipv6,
        mesh_prefix,
        wg_listen_port: config.wg_listen_port,
        node_name: config.node_name.clone(),
        public_endpoint: config.public_endpoint,
        peering_port: config.peering_port,
        peers: vec![],
        region: Some(region.clone()),
        zone: Some(zone.clone()),
        metrics: Default::default(),
    };
    store::save(&state)?;

    let sp = ui::spinner("Starting daemon...");
    ui::step_ok(&sp, &format!("Mesh '{}' created", config.mesh_name));
    ui::info_line("Node", &format!("{} ({mesh_ipv6})", config.node_name));
    ui::info_line("Region", &region);
    ui::info_line("Zone", &zone);
    println!();
    println!("  \u{26a0} Peering is not active. New nodes cannot join yet.");
    println!("    To accept nodes with a PIN:  syfrah fabric peering start --pin <PIN>");
    println!("    To approve manually:         syfrah fabric peering start");

    let my_record = build_record(
        &config.node_name,
        &wg_keypair,
        endpoint,
        mesh_ipv6,
        Some(&region),
        Some(&zone),
    );
    Ok(DaemonReady {
        my_record,
        wg_keypair,
        mesh_secret,
        peering_port: config.peering_port,
    })
}

/// Run the init flow: create a new mesh and run daemon (foreground).
pub async fn run_init(config: DaemonConfig) -> anyhow::Result<()> {
    let ready = setup_init(&config)?;
    println!();
    println!("Run 'syfrah fabric peering' to accept new nodes.");
    println!("Running daemon... (Ctrl+C to stop)");
    run_daemon(
        ready.my_record,
        &ready.wg_keypair,
        ready.mesh_secret,
        ready.peering_port,
    )
    .await
}

/// Auto-init: create mesh if none exists, used by `syfrah fabric peering` on a fresh node.
pub fn auto_init(
    node_name: &str,
    wg_port: u16,
    peering_port: u16,
) -> anyhow::Result<(MeshSecret, KeyPair)> {
    let tuning = config::load_tuning().unwrap_or_default();
    wg::set_interface_name(&tuning.interface_name);

    let mesh_secret = MeshSecret::generate();
    let wg_keypair = wg::generate_keypair();

    let mesh_prefix = derive_prefix_from_secret(&mesh_secret);
    let mesh_ipv6 = addressing::derive_node_address(&mesh_prefix, wg_keypair.public.as_bytes());

    let sp = ui::spinner("Setting up WireGuard interface...");
    wg::setup_interface(&wg_keypair, wg_port, mesh_ipv6)?;
    ui::step_ok(
        &sp,
        &format!("Interface {} up ({mesh_ipv6})", wg::interface_name()),
    );

    let (region, zone) = resolve_region_zone(None, None, &[]);

    let state = NodeState {
        mesh_name: node_name.to_string(),
        mesh_secret: mesh_secret.to_string(),
        wg_private_key: wg_keypair.private.to_base64(),
        wg_public_key: wg_keypair.public.to_base64(),
        mesh_ipv6,
        mesh_prefix,
        wg_listen_port: wg_port,
        node_name: node_name.to_string(),
        public_endpoint: None,
        peering_port,
        peers: vec![],
        region: Some(region),
        zone: Some(zone),
        metrics: Default::default(),
    };
    store::save(&state)?;

    let sp = ui::spinner("Auto-creating mesh...");
    ui::step_ok(&sp, "Mesh auto-created");
    ui::info_line("Secret", &mesh_secret.to_string());
    ui::info_line("Node", &format!("{node_name} ({mesh_ipv6})"));

    Ok((mesh_secret, wg_keypair))
}

/// Setup the join flow: send join request, save state, print info.
/// Returns a DaemonReady that can be passed to run_daemon.
///
/// On failure, any partial state (WireGuard interface, persisted state) is
/// rolled back so the user can retry without running `leave` first.
pub async fn setup_join(
    target: SocketAddr,
    config: &DaemonConfig,
    pin: Option<String>,
) -> anyhow::Result<DaemonReady> {
    if store::exists() {
        anyhow::bail!("mesh state already exists. Run 'syfrah fabric leave' first.");
    }

    let tuning = config::load_tuning().unwrap_or_default();
    wg::set_interface_name(&tuning.interface_name);

    let sp = ui::spinner(&format!("Connecting to {target}..."));
    let wg_keypair = wg::generate_keypair();
    let endpoint = resolve_endpoint(config);

    // Send region/zone in the request so the leader can store them.
    // If the user provided explicit values, include them; otherwise send
    // the region default and leave zone as None so the leader can
    // auto-generate it from its peer list.
    let req_region = Some(
        config
            .region
            .as_deref()
            .unwrap_or(DEFAULT_REGION)
            .to_string(),
    );
    if config.region.is_none() {
        ui::warn("No --region specified; using 'default'. Set --region to label this node.");
    }
    let req_zone = config.zone.clone();

    let mut request = syfrah_core::mesh::JoinRequest {
        request_id: peering::generate_request_id(),
        node_name: config.node_name.clone(),
        wg_public_key: wg_keypair.public.to_base64(),
        endpoint,
        wg_listen_port: config.wg_listen_port,
        pin,
        region: req_region,
        zone: req_zone,
        timestamp: 0,
        signature: String::new(),
    };
    // Sign the request with the WireGuard private key to prove possession.
    let wg_private_bytes: [u8; 32] = {
        let raw = wg_keypair.private.as_bytes();
        let mut buf = [0u8; 32];
        buf.copy_from_slice(raw);
        buf
    };
    syfrah_core::mesh::sign_join_request(&mut request, &wg_private_bytes);
    ui::step_ok(&sp, &format!("Connected to {target}"));

    let sp = ui::spinner("Waiting for approval...");
    // For the join handshake, we use a permissive TLS client config that skips
    // server certificate verification. The joiner does not yet know the mesh secret
    // (that arrives in the JoinResponse), so it cannot verify the mesh-derived cert.
    // The PIN exchange provides authentication at this stage.
    let join_tls_config = {
        let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(danger::NoCertVerifier))
            .with_no_client_auth();
        Arc::new(cfg)
    };
    let response = match peering::send_join_request(target, request, Some(join_tls_config)).await {
        Ok(resp) => resp,
        Err(e) => {
            ui::step_fail(&sp, &format!("Failed: {e}"));
            return Err(map_join_error(e, target));
        }
    };

    if !response.accepted {
        let reason = response.reason.unwrap_or_else(|| "no reason given".into());
        ui::step_fail(&sp, &format!("Rejected: {reason}"));
        anyhow::bail!("Join request rejected: {reason}");
    }
    ui::step_ok(&sp, "Approved");

    // Everything below writes state — wrap in rollback on error.
    match finalize_join(config, &wg_keypair, endpoint, &response).await {
        Ok(ready) => Ok(ready),
        Err(e) => {
            warn!(flow = "join", error = %e, "join failed after approval, rolling back");
            rollback_join_state();
            Err(e)
        }
    }
}

/// Finalize the join after approval: setup WG interface, save state, print info.
/// Separated from `setup_join` so the caller can rollback on any error here.
async fn finalize_join(
    config: &DaemonConfig,
    wg_keypair: &KeyPair,
    endpoint: SocketAddr,
    response: &syfrah_core::mesh::JoinResponse,
) -> anyhow::Result<DaemonReady> {
    let mesh_secret_str = response
        .mesh_secret
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("accepted but no mesh secret"))?;
    let mesh_secret: MeshSecret = mesh_secret_str
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid mesh secret: {e}"))?;
    let mesh_name = response.mesh_name.as_deref().unwrap_or("mesh");
    let mesh_prefix = response
        .mesh_prefix
        .ok_or_else(|| anyhow::anyhow!("accepted but no mesh prefix"))?;

    let mesh_ipv6 = addressing::derive_node_address(&mesh_prefix, wg_keypair.public.as_bytes());

    // Region/zone: use provided or auto-generate from existing peers
    let (region, zone) = resolve_region_zone(
        config.region.as_deref(),
        config.zone.as_deref(),
        &response.peers,
    );

    let sp = ui::spinner("Setting up WireGuard interface...");
    wg::setup_interface(wg_keypair, config.wg_listen_port, mesh_ipv6)?;
    ui::step_ok(
        &sp,
        &format!("Interface {} up ({mesh_ipv6})", wg::interface_name()),
    );
    info!(flow = "join", node = %config.node_name, "wireguard interface up");

    if !response.peers.is_empty() {
        let sp = ui::spinner("Syncing peers...");
        info!(
            flow = "join",
            count = response.peers.len(),
            "applying peers from join response"
        );
        let tuning = config::load_tuning().unwrap_or_default();
        if let Err(e) = wg::apply_peers(
            &wg_keypair.public,
            &response.peers,
            tuning.keepalive_interval,
        ) {
            warn!(flow = "join", error = %e, "failed to apply peers");
        }
        ui::step_ok(&sp, &format!("{} peers configured", response.peers.len()));
    }

    let state = NodeState {
        mesh_name: mesh_name.to_string(),
        mesh_secret: mesh_secret_str.clone(),
        wg_private_key: wg_keypair.private.to_base64(),
        wg_public_key: wg_keypair.public.to_base64(),
        mesh_ipv6,
        mesh_prefix,
        wg_listen_port: config.wg_listen_port,
        node_name: config.node_name.clone(),
        public_endpoint: config.public_endpoint,
        peering_port: config.peering_port,
        peers: response.peers.clone(),
        region: Some(region.clone()),
        zone: Some(zone.clone()),
        metrics: Default::default(),
    };
    store::save(&state)?;

    let sp = ui::spinner("Starting daemon...");
    ui::step_ok(&sp, &format!("Joined mesh '{mesh_name}'"));
    match response.approved_by.as_deref() {
        Some("pin") => ui::info_line("Approval", "PIN accepted by the target node"),
        Some("manual") => {
            ui::info_line("Approval", "Approved by the target node (manual approval)")
        }
        _ => ui::info_line("Approval", "Approved by the target node"),
    }
    ui::info_line("Node", &format!("{} ({mesh_ipv6})", config.node_name));
    ui::info_line("Region", &region);
    ui::info_line("Zone", &zone);
    println!();
    ui::warn("The mesh secret is stored in ~/.syfrah/state.json");
    println!("    Keep this file safe \u{2014} it grants full mesh access.");

    let my_record = build_record(
        &config.node_name,
        wg_keypair,
        endpoint,
        mesh_ipv6,
        Some(&region),
        Some(&zone),
    );
    Ok(DaemonReady {
        my_record,
        wg_keypair: KeyPair::from_private(wg_keypair.private.clone()),
        mesh_secret,
        peering_port: config.peering_port,
    })
}

/// Roll back any partial state left by a failed join attempt.
/// Tears down WireGuard interface and clears persisted state so the user
/// can retry without running `leave` first.
fn rollback_join_state() {
    if let Err(e) = wg::teardown_interface() {
        debug!(error = %e, "rollback: no interface to tear down (expected)");
    }
    if store::exists() {
        if let Err(e) = store::clear() {
            warn!(error = %e, "rollback: failed to clear state");
        }
    }
}

/// Map peering errors during join to user-friendly messages.
fn map_join_error(err: peering::PeeringError, target: SocketAddr) -> anyhow::Error {
    match &err {
        peering::PeeringError::Io(io_err) if io_err.kind() == std::io::ErrorKind::UnexpectedEof => {
            anyhow::anyhow!(
                "Connection closed by {target}. The target node may not have peering active.\n  \
                 Ask the operator to run: syfrah fabric peering start"
            )
        }
        peering::PeeringError::Io(io_err)
            if io_err.kind() == std::io::ErrorKind::ConnectionRefused
                || io_err.kind() == std::io::ErrorKind::ConnectionReset =>
        {
            anyhow::anyhow!(
                "Could not connect to {target}. \
                 Is the target node running with peering enabled?\n  \
                 Ask the operator to run: syfrah fabric peering start"
            )
        }
        peering::PeeringError::Io(_) => {
            anyhow::anyhow!(
                "Could not connect to {target}: {err}. \
                 Is the target node running with peering enabled?"
            )
        }
        peering::PeeringError::Timeout => {
            anyhow::anyhow!(
                "Connection to {target} timed out. The target node may not be reachable or peering may not be active.\n  \
                 Ask the operator to run: syfrah fabric peering start"
            )
        }
        peering::PeeringError::Tls(detail) => {
            anyhow::anyhow!(
                "TLS handshake failed with {target}. Verify the node is running a compatible version.\n  \
                 Detail: {detail}"
            )
        }
        _ => err.into(),
    }
}

/// Run the join flow: join mesh and run daemon (foreground).
pub async fn run_join(
    target: SocketAddr,
    config: DaemonConfig,
    pin: Option<String>,
) -> anyhow::Result<()> {
    let ready = setup_join(target, &config, pin).await?;
    println!("Running daemon... (Ctrl+C to stop)");
    run_daemon(
        ready.my_record,
        &ready.wg_keypair,
        ready.mesh_secret,
        ready.peering_port,
    )
    .await
}

/// Setup restart from saved state: load state, setup WG, print info.
/// Returns a DaemonReady that can be passed to run_daemon.
pub fn setup_start() -> anyhow::Result<DaemonReady> {
    let tuning = config::load_tuning().unwrap_or_default();
    wg::set_interface_name(&tuning.interface_name);

    let state = store::load().map_err(|_| crate::no_mesh_error())?;

    let mesh_secret: MeshSecret = state
        .mesh_secret
        .parse()
        .map_err(|e| anyhow::anyhow!("corrupt secret in state: {e}"))?;
    let wg_private = Key::from_base64(&state.wg_private_key)
        .map_err(|_| anyhow::anyhow!("corrupt WG private key in state"))?;
    let wg_keypair = KeyPair::from_private(wg_private);

    let sp = ui::spinner("Setting up WireGuard interface...");
    wg::setup_interface(&wg_keypair, state.wg_listen_port, state.mesh_ipv6)?;
    ui::step_ok(
        &sp,
        &format!(
            "Interface {} up ({})",
            wg::interface_name(),
            state.mesh_ipv6
        ),
    );

    if !state.peers.is_empty() {
        let sp_peers = ui::spinner("Syncing peers...");
        info!(
            flow = "start",
            count = state.peers.len(),
            "applying peers from saved state"
        );
        let tuning = config::load_tuning().unwrap_or_default();
        if let Err(e) = wg::apply_peers(&wg_keypair.public, &state.peers, tuning.keepalive_interval)
        {
            warn!(flow = "start", error = %e, "failed to apply saved peers");
        }
        ui::step_ok(
            &sp_peers,
            &format!("{} peers configured", state.peers.len()),
        );
    }

    let sp = ui::spinner("Starting daemon...");
    ui::step_ok(&sp, &format!("Restarting mesh '{}'", state.mesh_name));
    ui::info_line(
        "Node",
        &format!("{} ({})", state.node_name, state.mesh_ipv6),
    );

    let endpoint_addr = state
        .public_endpoint
        .unwrap_or_else(|| SocketAddr::new("0.0.0.0".parse().unwrap(), state.wg_listen_port));
    let my_record = build_record(
        &state.node_name,
        &wg_keypair,
        endpoint_addr,
        state.mesh_ipv6,
        state.region.as_deref(),
        state.zone.as_deref(),
    );
    Ok(DaemonReady {
        my_record,
        wg_keypair,
        mesh_secret,
        peering_port: state.peering_port,
    })
}

/// Restart daemon from saved state (foreground).
pub async fn run_start() -> anyhow::Result<()> {
    let ready = setup_start()?;
    println!("Running daemon... (Ctrl+C to stop)");
    run_daemon(
        ready.my_record,
        &ready.wg_keypair,
        ready.mesh_secret,
        ready.peering_port,
    )
    .await
}

/// Leave the mesh.
///
/// Returns `true` if there was a mesh to leave, `false` if nothing was configured.
pub async fn run_leave() -> anyhow::Result<bool> {
    // Stop the daemon first (if running) so it cannot recreate state files.
    if let Some(pid) = store::daemon_running() {
        #[cfg(unix)]
        {
            if store::is_syfrah_process(pid) {
                let sp = ui::spinner("Stopping daemon...");
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                // Wait for the daemon process to actually exit (up to 10s).
                // Just sleeping a fixed 2s was not enough — the daemon may
                // still hold file locks when we try to remove the state dir.
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
                    if !alive {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        // Force-kill if it didn't exit gracefully.
                        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                ui::step_ok(&sp, "Daemon stopped");
            }
        }
    }

    if !store::exists() {
        // Even when no mesh state exists, clean up any leftover runtime files
        // (PID file, control socket) that might remain from a partial cleanup.
        store::remove_pid();
        let _ = std::fs::remove_file(store::control_socket_path());
        return Ok(false);
    }

    let sp = ui::spinner("Tearing down WireGuard interface...");
    if let Err(e) = wg::teardown_interface() {
        ui::step_fail(&sp, &format!("Could not tear down interface: {e}"));
    } else {
        ui::step_ok(&sp, "Interface removed");
    }
    let sp = ui::spinner("Cleaning up state...");
    // Clear all state atomically: redb, JSON, PID file, control socket, and
    // any other files in ~/.syfrah. store::clear() removes the entire directory.
    // Retry once after a short delay if the first attempt fails (e.g. a file
    // lock was not yet released by the dying daemon process).
    if let Err(e) = store::clear() {
        debug!(error = %e, "first clear attempt failed, retrying");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        store::clear()?;
    }
    ui::step_ok(&sp, "Left the mesh. State cleared.");
    Ok(true)
}

/// The main daemon loop.
pub async fn run_daemon(
    my_record: PeerRecord,
    wg_keypair: &KeyPair,
    mesh_secret: MeshSecret,
    peering_port: u16,
) -> anyhow::Result<()> {
    // Ignore SIGPIPE so the daemon is not killed when writing to a
    // disconnected TCP socket (e.g. a join client that timed out).
    // The binary's main() sets SIGPIPE to SIG_DFL for CLI piping, but
    // a long-running daemon must not be terminated by broken pipes.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // Ensure ring is installed as the global CryptoProvider. This is needed
    // because rustls 0.23 no longer auto-selects a provider at runtime.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let tuning = config::load_tuning().unwrap_or_else(|e| {
        warn!("failed to load config.toml: {e}, using defaults");
        Tuning::default()
    });
    wg::set_interface_name(&tuning.interface_name);
    info!(
        "daemon tuning: health_check={}s reconcile={}s persist={}s unreachable={}s max_peers={} max_concurrent_announces={} announce_queue_size={}",
        tuning.health_check_interval.as_secs(),
        tuning.reconcile_interval.as_secs(),
        tuning.persist_interval.as_secs(),
        tuning.unreachable_timeout.as_secs(),
        tuning.max_peers,
        tuning.max_concurrent_announces,
        tuning.announce_queue_size,
    );

    // Load HTTP API config (includes /metrics endpoint).
    let api_config = http_api::load_api_config();

    // Load gRPC-compatible API config.
    let grpc_config = crate::grpc_api::load_grpc_config();

    // Load gateway config (dedicated gateway node role).
    let gateway_config = config::load_gateway_config();
    if gateway_config.enabled {
        info!(
            "gateway mode enabled, external API will listen on {} (TLS)",
            gateway_config.bind_address
        );
    }

    let announce_semaphore = Arc::new(Semaphore::new(tuning.max_concurrent_announces));
    let max_peers = tuning.max_peers;
    let keepalive_interval = tuning.keepalive_interval;

    // Acquire exclusive PID file lock. The returned file handle must be kept
    // alive for the entire daemon lifetime — dropping it releases the flock.
    let _pid_lock = store::write_pid()?;
    events::emit(
        EventType::DaemonStarted,
        None,
        None,
        None,
        Some(tuning.max_events),
    );
    audit_log::emit(AuditEventType::DaemonStarted, None, None, None);

    let wg_pubkey = wg_keypair.public.clone();
    let peering_state = Arc::new(PeeringState::with_limits(
        tuning.max_concurrent_connections,
        tuning.max_pending_joins,
    ));
    // Normalize to V1 derivation: when a node joins or restarts, the secret is
    // parsed from a string which always yields V1.  The init node gets V2 from
    // generate(), but every other node will have V1.  We must use the same
    // derivation everywhere so encryption keys match across the mesh.
    let mesh_secret = syfrah_core::secret::MeshSecret::from_bytes(*mesh_secret.as_bytes());
    let enc_key = mesh_secret.encryption_key();

    // Build TLS configuration from the raw mesh secret for peering connections.
    // Use as_bytes() (not encryption_key()) so TLS certs are identical regardless
    // of the derivation version (V1 vs V2) — all nodes share the same raw secret.
    let mesh_secret_bytes: [u8; 32] = *mesh_secret.as_bytes();
    let tls_server_config = peering::build_tls_server_config(&mesh_secret_bytes)
        .map_err(|e| anyhow::anyhow!("failed to build TLS server config: {e}"))?;
    let tls_client_config = peering::build_tls_client_config(&mesh_secret_bytes)
        .map_err(|e| anyhow::anyhow!("failed to build TLS client config: {e}"))?;

    let metrics_received = Arc::new(AtomicU64::new(0));
    let metrics_reconciliations = Arc::new(AtomicU64::new(0));
    let metrics_unreachable = Arc::new(AtomicU64::new(0));
    let metrics_announces_dropped = Arc::new(AtomicU64::new(0));
    let metrics_announces_queued = Arc::new(AtomicU64::new(0));
    let metrics_announces_queue_full = Arc::new(AtomicU64::new(0));
    let metrics_peer_limit_reached = Arc::new(AtomicU64::new(0));
    let metrics_health_check_failures = Arc::new(AtomicU64::new(0));
    let metrics_reconcile_failures = Arc::new(AtomicU64::new(0));
    let metrics_store_failures = Arc::new(AtomicU64::new(0));
    let daemon_started = now();

    // on_accepted callback: when a peer is accepted (manual or PIN), add to WG + store + announce
    let accepted_wg_pubkey = wg_pubkey.clone();
    let accepted_recv = metrics_received.clone();
    let accepted_recon = metrics_reconciliations.clone();
    let accepted_enc_key = enc_key;
    let accepted_peering_port = peering_port;
    let accepted_max_events = tuning.max_events;
    let accepted_max_peers = max_peers;
    let accepted_keepalive = keepalive_interval;
    let accepted_peer_limit_counter = metrics_peer_limit_reached.clone();
    let accepted_store_failures = metrics_store_failures.clone();
    let accepted_tls_client = tls_client_config.clone();
    let accepted_my_record = my_record.clone();
    let accepted_announce_cfg = tuning.announcements.clone();
    let on_accepted: peering::OnAccepted = Arc::new(move |new_record| {
        accepted_recv.fetch_add(1, Ordering::Relaxed);
        let pubkey = accepted_wg_pubkey.clone();
        let recon = accepted_recon.clone();
        let record = new_record.clone();
        let enc = accepted_enc_key;
        let pp = accepted_peering_port;
        let max_ev = accepted_max_events;
        let mp = accepted_max_peers;
        let ka = accepted_keepalive;
        let tls_cfg = accepted_tls_client.clone();
        let plr = accepted_peer_limit_counter.clone();
        let sf = accepted_store_failures.clone();
        let wave_source = accepted_my_record.clone();
        let wave_cfg = accepted_announce_cfg.clone();
        tokio::spawn(async move {
            // Purge stale peers with the same node name but different WG key.
            // This prevents phantom peer accumulation from repeated init/join
            // cycles of the same node (issue #285).
            match store::purge_stale_peers_by_name(&record.name, &record.wg_public_key) {
                Ok(0) => {}
                Ok(n) => {
                    info!(
                        peer = %sanitize(&record.name),
                        purged = n,
                        "purged stale peer records with same node name"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "on_accepted: failed to purge stale peers");
                }
            }

            // Reject peers whose endpoint is 0.0.0.0 — WireGuard cannot
            // send packets to an unspecified address (issue #285).
            if record.endpoint.ip().is_unspecified() {
                warn!(
                    peer = %sanitize(&record.name),
                    endpoint = %record.endpoint,
                    "on_accepted: rejecting peer with unspecified (0.0.0.0) endpoint"
                );
                return;
            }

            // Reject peers whose endpoint matches our own public IP —
            // a node sending WG packets to itself causes Invalid MAC
            // loops and disrupts the mesh (issue #285).
            if let Ok(state) = store::load() {
                if let Some(my_endpoint) = state.public_endpoint {
                    if record.endpoint.ip() == my_endpoint.ip() {
                        warn!(
                            peer = %sanitize(&record.name),
                            endpoint = %record.endpoint,
                            my_ip = %my_endpoint.ip(),
                            "on_accepted: rejecting peer with self-referencing endpoint"
                        );
                        return;
                    }
                }
            }

            // Check store peer count + existence in a single read transaction (fail closed: skip on error).
            // The store is the source of truth for the peer limit — not the WG kernel
            // interface — because the store is always reachable (no root required for reads)
            // and its count survives daemon restarts. The TOCTOU window is accepted as a
            // soft limit (see wg::upsert_peer_bounded doc).
            let (current_count, exists) = match store::peer_count_and_exists(&record.wg_public_key)
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "on_accepted: failed to read peer count, skipping upsert");
                    sf.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            let threshold = mp * 4 / 5; // 80%
            if current_count >= threshold && current_count < mp {
                warn!(
                    current = current_count,
                    max = mp,
                    "peer count approaching limit ({}%)",
                    current_count * 100 / mp
                );
            }

            // Add to WG (bounded)
            match wg::upsert_peer_bounded(&pubkey, &record, mp, current_count, exists, ka) {
                Err(e) => {
                    if matches!(e, wg::WgError::PeerLimitExceeded(_, _)) {
                        plr.fetch_add(1, Ordering::Relaxed);
                        let _ = store::inc_metric("peer_limit_reached", 1);
                        events::emit(
                            EventType::PeerLimitReached,
                            Some(&sanitize(&record.name)),
                            Some(&record.endpoint.to_string()),
                            Some(&format!("max_peers={mp}")),
                            Some(max_ev),
                        );
                    }
                    warn!(peer = %sanitize(&record.name), endpoint = %record.endpoint, error = %e, "failed to add peer to WG");
                }
                Ok(()) => {
                    recon.fetch_add(1, Ordering::Relaxed);
                    info!(peer = %sanitize(&record.name), endpoint = %record.endpoint, "peer accepted and added to WG");
                    events::emit(
                        EventType::PeerActive,
                        Some(&sanitize(&record.name)),
                        Some(&record.endpoint.to_string()),
                        Some(&format!("mesh_ipv6={}", record.mesh_ipv6)),
                        Some(max_ev),
                    );
                }
            }
            // Save to store (bounded — reject if over limit)
            match store::upsert_peer_bounded(&record, mp) {
                Ok(false) => {
                    warn!(peer = %sanitize(&record.name), max = mp, "peer limit reached, not persisting new peer");
                    plr.fetch_add(1, Ordering::Relaxed);
                    let _ = store::inc_metric("peer_limit_reached", 1);
                    events::emit(
                        EventType::PeerLimitReached,
                        Some(&sanitize(&record.name)),
                        Some(&record.endpoint.to_string()),
                        Some(&format!("store max_peers={mp}")),
                        Some(max_ev),
                    );
                }
                Err(e) => {
                    warn!(peer = %sanitize(&record.name), error = %e, "failed to persist peer");
                }
                Ok(true) => {}
            }
            // Announce to existing peers using topology-aware waves.
            let known = match store::get_peers() {
                Ok(peers) => peers,
                Err(e) => {
                    error!(error = %e, "on_accepted: failed to load peers for announce, skipping");
                    sf.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            let (_ok, failed) = peering::announce_peer_in_waves(
                &record,
                &wave_source,
                &known,
                &enc,
                pp,
                Some(tls_cfg),
                &wave_cfg,
            )
            .await;
            if failed > 0 {
                let _ = store::inc_metric("announcements_failed", failed as u64);
                events::emit(
                    EventType::PeerAnnounceFailed,
                    Some(&sanitize(&record.name)),
                    None,
                    Some(&format!("failed_count={failed}")),
                    Some(max_ev),
                );
            }
        });
    });

    // Shared mutable state for secret rotation: these are wrapped in RwLock so
    // that the control handler and the on_secret_rotation callback can swap in
    // new values when the mesh secret is rotated at runtime.
    let shared_mesh_secret = Arc::new(tokio::sync::RwLock::new(mesh_secret.clone()));
    let shared_tls_client = Arc::new(tokio::sync::RwLock::new(tls_client_config.clone()));

    // Control handler — wrap the fabric handler in a LayerRouter so the
    // daemon multiplexes all layers over a single socket.
    let ctrl_handler = Arc::new(DaemonFabricHandler {
        peering_state: peering_state.clone(),
        mesh_secret: shared_mesh_secret.clone(),
        my_record: my_record.clone(),
        wg_pubkey: wg_pubkey.clone(),
        peering_port,
        on_accepted: on_accepted.clone(),
        tls_client_config: shared_tls_client.clone(),
        max_events: tuning.max_events,
        max_peers,
    });

    // Share the handler with the gRPC API server.
    let grpc_handler: Arc<dyn FabricHandler> = ctrl_handler.clone();

    let fabric_layer_handler = FabricLayerHandler::new(ctrl_handler);
    let mut router = LayerRouter::new();
    router.register("fabric", Arc::new(fabric_layer_handler));

    // -- Org layer integration -----------------------------------------------
    //
    // Open the org database once and share it between the org control handler
    // and the compute layer. This avoids the redb exclusive-lock contention
    // that previously prevented CLI commands from accessing org.redb while the
    // daemon was running.
    let mut shared_sg_rule_store: Option<Arc<syfrah_org::SgRuleStore>> = None;
    let shared_org_store: Option<Arc<syfrah_org::OrgStore>> =
        match syfrah_state::LayerDb::open("org") {
            Ok(org_db) => {
                let store = Arc::new(syfrah_org::OrgStore::new(org_db));

                // Register the org layer handler so CLI org/project/env/vpc/subnet
                // commands are routed through the daemon's control socket.
                let mut org_handler = syfrah_org::OrgLayerHandler::new(Arc::clone(&store));

                // Open the SG rule store for rule management CLI commands.
                if let Ok(sg_rules_db) = syfrah_state::LayerDb::open("sg_rules") {
                    let sg_rule_store = Arc::new(syfrah_org::SgRuleStore::new(sg_rules_db));
                    org_handler = org_handler.with_sg_rule_store(Arc::clone(&sg_rule_store));
                    shared_sg_rule_store = Some(sg_rule_store);
                    info!("sg_rules store initialised");
                }

                router.register("org", Arc::new(org_handler));

                info!("org layer initialised");
                Some(store)
            }
            Err(e) => {
                warn!("org layer init failed (non-fatal): {e}");
                None
            }
        };

    // -- Hypervisor store (shared) -------------------------------------------
    //
    // Open the hypervisor database once so it can be shared between the
    // compute layer (VM region/zone metadata) and the hypervisor handler
    // (CLI commands + auto-discovery).  redb only allows a single open
    // handle per database file, so we must avoid opening it twice.
    let shared_hypervisor_store: Option<Arc<syfrah_org::HypervisorStore>> =
        match syfrah_state::LayerDb::open("hypervisor") {
            Ok(hv_db) => {
                let store = Arc::new(syfrah_org::HypervisorStore::new(hv_db));
                info!("hypervisor store opened");
                Some(store)
            }
            Err(e) => {
                warn!("hypervisor store init failed (non-fatal): {e}");
                None
            }
        };

    // -- Compute layer integration ------------------------------------------
    //
    // Initialise VmManager, reconnect existing VMs, start the monitor loop,
    // and register the compute handler in the router.
    //
    // This block runs unconditionally on ALL daemon startup paths (init, join,
    // AND start) so that `syfrah compute` commands are always available.
    info!("compute layer: starting initialisation");
    let shared_vm_manager: Option<Arc<syfrah_compute::VmManager>>;
    {
        // Pre-create prerequisite directories so VmManager::new() succeeds
        // even on a fresh node (join path) where install.sh was not run.
        for dir in &[
            "/opt/syfrah/images",
            "/opt/syfrah/kernels",
            "/opt/syfrah/instances",
            "/run/syfrah/vms",
        ] {
            if let Err(e) = std::fs::create_dir_all(dir) {
                warn!("compute layer: failed to create {dir}: {e}");
            }
        }

        // Warn about missing runtime prerequisites (non-fatal).
        if !syfrah_compute::runtime_container::container_binaries_available() {
            warn!("warning: no container runtime found (crun, runsc). Install crun for container VM support.");
        }
        if std::path::Path::new("/dev/kvm").exists() {
            // KVM is available — check for cloud-hypervisor.
            if syfrah_compute::binary::resolve_binary(None).is_err() {
                warn!("warning: cloud-hypervisor binary not found. Install it for KVM VM support.");
            }
        }

        let compute_config = syfrah_compute::ComputeConfig::default();
        match syfrah_compute::VmManager::new(compute_config) {
            Ok(mut vm_manager) => {
                // Wire networking into VmManager: reuse the shared org store
                // and open ipam/placement stores.
                let net_ok = match (
                    shared_org_store.clone(),
                    syfrah_state::LayerDb::open("ipam"),
                    syfrah_state::LayerDb::open("placement"),
                ) {
                    (Some(org_store), Ok(ipam_db), Ok(placement_db)) => {
                        let ipam_store = Arc::new(syfrah_org::ipam::IpamStore::new(ipam_db));
                        let placement_store =
                            Arc::new(syfrah_org::PlacementStore::new(placement_db));
                        let backend: Arc<dyn syfrah_overlay::NetworkBackend> =
                            Arc::new(syfrah_overlay::LinuxBackend::new());
                        let local_node = my_record.mesh_ipv6.to_string();

                        // Set up network cleanup callbacks for VM delete.
                        let ipam_for_release = Arc::clone(&ipam_store);
                        let placement_for_remove = Arc::clone(&placement_store);
                        let placement_for_counter = Arc::clone(&placement_store);

                        #[allow(clippy::type_complexity)]
                        let bridge_counter: Arc<
                            dyn Fn(&str, &str) -> usize + Send + Sync,
                        > = {
                            Arc::new(move |vpc_id, vm_id| {
                                placement_for_counter
                                    .list_by_vpc(vpc_id)
                                    .unwrap_or_default()
                                    .iter()
                                    .filter(|p| p.vm_id != vm_id)
                                    .count()
                            })
                        };
                        #[allow(clippy::type_complexity)]
                        let ipam_releaser: Arc<
                            dyn Fn(&str, &str, &str) -> Result<(), String> + Send + Sync,
                        > = Arc::new(move |subnet_id, subnet_cidr, ip| {
                            ipam_for_release
                                .release_ip(subnet_id, subnet_cidr, ip)
                                .map_err(|e| e.to_string())
                        });
                        #[allow(clippy::type_complexity)]
                        let placement_remover: Arc<
                            dyn Fn(&str, &str) -> Result<(), String> + Send + Sync,
                        > = Arc::new(move |vpc_id, vm_id| {
                            placement_for_remove
                                .remove_placement(vpc_id, vm_id)
                                .map_err(|e| e.to_string())
                        });

                        // Apply infrastructure protection rules before any
                        // VM rules — blocks VMs from reaching VXLAN, WireGuard
                        // and peering ports on the host and in forwarded traffic.
                        if let Err(e) = backend.apply_infra_protection().await {
                            warn!("compute: failed to apply infra protection rules: {e}");
                        }

                        vm_manager.set_network(
                            Arc::clone(&backend),
                            bridge_counter,
                            ipam_releaser,
                            placement_remover,
                        );
                        vm_manager.set_network_setup(
                            org_store,
                            ipam_store,
                            placement_store,
                            local_node,
                        );

                        // Wire SG rule store so nftables rules are generated
                        // from actual SG rules instead of hardcoded defaults.
                        if let Some(ref sg_rs) = shared_sg_rule_store {
                            vm_manager.set_sg_rule_store(Arc::clone(sg_rs));
                        }

                        info!("compute: networking wired (IPAM, bridge, VXLAN, TAP, nftables)");
                        true
                    }
                    (org_res, ipam_res, placement_res) => {
                        if org_res.is_none() {
                            warn!("compute: org store not available");
                        }
                        if let Err(e) = ipam_res {
                            warn!("compute: failed to open ipam store: {e}");
                        }
                        if let Err(e) = placement_res {
                            warn!("compute: failed to open placement store: {e}");
                        }
                        warn!("compute: networking not available (store init failed)");
                        false
                    }
                };
                let _ = net_ok;

                // Wire the hypervisor store so VMs get hypervisor/region/zone
                // metadata at creation time.
                if let Some(ref hv_store) = shared_hypervisor_store {
                    vm_manager.set_hypervisor_store(Arc::clone(hv_store), my_record.name.clone());
                    info!("compute: hypervisor store wired (region/zone in vm get)");
                }

                let vm_manager = Arc::new(vm_manager);

                // Reconnect VMs that survived a daemon restart.
                match vm_manager.reconnect().await {
                    Ok(summary) => {
                        if summary.recovered_count > 0 || !summary.failed.is_empty() {
                            info!(
                                recovered = summary.recovered_count,
                                failed = summary.failed.len(),
                                orphans = summary.orphans_cleaned.len(),
                                "compute: VM reconnect complete"
                            );
                        }
                    }
                    Err(e) => {
                        warn!("compute: reconnect failed: {e}");
                    }
                }

                // Start the background health-check monitor.
                vm_manager.start_monitor();

                // Register the compute layer handler.
                let compute_handler =
                    syfrah_compute::ComputeLayerHandler::new(Arc::clone(&vm_manager));
                router.register("compute", Arc::new(compute_handler));

                info!("compute layer initialised");
                shared_vm_manager = Some(vm_manager);
            }
            Err(e) => {
                // Compute init failure is non-fatal: the daemon still runs for
                // fabric operations, but compute CLI commands will get
                // "unknown layer" errors.
                warn!("compute layer init failed (non-fatal): {e}");
                shared_vm_manager = None;
            }
        }
    }

    // -- Hypervisor auto-discovery + handler registration ----------------------
    //
    // If the hypervisor store was opened successfully, run auto-discovery
    // (if KVM is present) and always register the handler so that CLI
    // commands like `syfrah hypervisor register` work on every node.
    if let Some(ref hv_store) = shared_hypervisor_store {
        let region = my_record.region.as_deref().unwrap_or("default");
        let zone = my_record.zone.as_deref().unwrap_or("default");
        let fabric_ipv6 = my_record.mesh_ipv6.to_string();
        let public_ip = my_record.endpoint.ip().to_string();
        let fabric_node_id = wg_pubkey.to_base64();

        let _hv_name = syfrah_org::discovery::discover_hypervisor(
            hv_store,
            &my_record.name,
            &fabric_node_id,
            region,
            zone,
            &public_ip,
            &fabric_ipv6,
        );
        // Register the hypervisor layer handler for CLI commands.
        let hv_handler =
            syfrah_org::HypervisorLayerHandler::new(Arc::clone(hv_store), my_record.name.clone());
        router.register("hypervisor", Arc::new(hv_handler));
        info!("hypervisor layer initialised");
    }

    // Register the forge layer handler on the control socket so future
    // `syfrah forge` CLI commands can be routed through the daemon.
    // Currently returns "not implemented" — real Forge operations use HTTP.
    router.register("forge", Arc::new(syfrah_forge::ForgeHandler));

    let router = Arc::new(router);

    let control_path = store::control_socket_path();
    let mut control_task = tokio::spawn(async move {
        control::start_control_listener(&control_path, router).await;
    });

    // -- Forge HTTP API server -----------------------------------------------
    //
    // Migration: daemon.rs starts the Forge HTTP server alongside the control
    // socket. Both coexist — CLI commands still go through the control socket
    // (fabric/org/compute handlers), while new API consumers use HTTP on
    // syfrah0:7100. The daemon is Forge's process entry point.
    let (forge_shutdown_tx, forge_shutdown_rx) = tokio::sync::watch::channel(false);
    let forge_task = {
        // Open the task store for Forge operations.
        let forge_task_store = match syfrah_state::LayerDb::open("forge_tasks") {
            Ok(db) => {
                let store = Arc::new(syfrah_forge::task::TaskStore::new(db));
                info!("forge: task store initialised");
                Some(store)
            }
            Err(e) => {
                warn!("forge: task store init failed (non-fatal): {e}");
                None
            }
        };

        // Initialize capacity tracker.
        let forge_capacity = Arc::new(syfrah_forge::capacity::CapacityTracker::new());
        info!(
            "forge: capacity tracker initialised (vcpus={}, memory={}MB)",
            forge_capacity.allocatable_vcpus(),
            forge_capacity.allocatable_memory_mb()
        );

        let forge_state = std::sync::Arc::new(syfrah_forge::api::ForgeState {
            started_at: std::time::Instant::now(),
            task_store: forge_task_store,
            vm_manager: shared_vm_manager.clone(),
            capacity: Some(forge_capacity),
            org_store: shared_org_store.clone(),
            network_backend: Some(std::sync::Arc::new(syfrah_overlay::LinuxBackend::new())
                as std::sync::Arc<dyn syfrah_overlay::NetworkBackend>),
            generation_tracker: Some(std::sync::Arc::new(
                syfrah_forge::generation::GenerationTracker::new(),
            )),
            nic_registry: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            nat_gw_registry: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            fdb_registry: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        });

        let bind_addr: std::net::SocketAddr =
            std::net::SocketAddr::new(std::net::IpAddr::V6(my_record.mesh_ipv6), 7100);

        tokio::spawn(async move {
            if let Err(e) =
                syfrah_forge::ForgeServer::serve(bind_addr, forge_state, forge_shutdown_rx).await
            {
                warn!("Forge HTTP API error: {e}");
            }
        })
    };

    // Bounded retry queue for announces that cannot be processed immediately.
    let (announce_queue_tx, announce_queue_rx) =
        tokio::sync::mpsc::channel::<PeerRecord>(tuning.announce_queue_size);
    let announce_queue_rx = Arc::new(tokio::sync::Mutex::new(announce_queue_rx));

    // on_announce callback: when a peer announce arrives from existing mesh member
    let announce_wg_pubkey = wg_pubkey.clone();
    let announce_recv = metrics_received.clone();
    let announce_recon = metrics_reconciliations.clone();
    let announce_max_events = tuning.max_events;
    let announce_sem = announce_semaphore.clone();
    let announce_dropped = metrics_announces_dropped.clone();
    let announce_queued = metrics_announces_queued.clone();
    let announce_queue_full = metrics_announces_queue_full.clone();
    let announce_queue_tx = announce_queue_tx.clone();
    let announce_peer_limit = metrics_peer_limit_reached.clone();
    let announce_max_peers = max_peers;
    let announce_keepalive = keepalive_interval;
    let announce_store_failures = metrics_store_failures.clone();
    let announce_enc_key = enc_key;
    let announce_peering_port = peering_port;
    let announce_tls_client = tls_client_config.clone();
    let my_endpoint_for_announce = my_record.endpoint;
    let announce_my_record = my_record.clone();
    let announce_wave_cfg = tuning.announcements.clone();
    let on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync> = Arc::new(move |record| {
        announce_recv.fetch_add(1, Ordering::Relaxed);
        events::emit(
            EventType::PeerAnnounceReceived,
            Some(&sanitize(&record.name)),
            Some(&record.endpoint.to_string()),
            Some(&format!("mesh_ipv6={}", record.mesh_ipv6)),
            Some(announce_max_events),
        );

        // Reject announced peers with 0.0.0.0 endpoint — WireGuard cannot
        // route packets to an unspecified address (issue #285).
        if record.endpoint.ip().is_unspecified() {
            warn!(
                peer = %sanitize(&record.name),
                endpoint = %record.endpoint,
                "on_announce: dropping peer with unspecified (0.0.0.0) endpoint"
            );
            return;
        }

        // Reject announced peers whose endpoint matches our own public IP.
        // A node sending WG packets to itself causes Invalid MAC loops (issue #285).
        if !my_endpoint_for_announce.ip().is_unspecified()
            && record.endpoint.ip() == my_endpoint_for_announce.ip()
        {
            warn!(
                peer = %sanitize(&record.name),
                endpoint = %record.endpoint,
                "on_announce: dropping peer with self-referencing endpoint"
            );
            return;
        }

        // Bound concurrent announce processing with a semaphore.
        // When the semaphore is full, queue the announce for retry instead of dropping it.
        let permit = match announce_sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // Try to queue for retry instead of dropping immediately.
                match announce_queue_tx.try_send(record.clone()) {
                    Ok(()) => {
                        announce_queued.fetch_add(1, Ordering::Relaxed);
                        let _ = store::inc_metric("announces_queued", 1);
                        debug!(peer = %sanitize(&record.name), "announce queued for retry (semaphore full)");
                        events::emit(
                            EventType::AnnounceQueued,
                            Some(&record.name),
                            Some(&record.endpoint.to_string()),
                            Some("semaphore full, queued for retry"),
                            Some(announce_max_events),
                        );
                    }
                    Err(_) => {
                        // Queue is also full — drop as last resort.
                        warn!(peer = %sanitize(&record.name), "announce queue full, dropping");
                        announce_dropped.fetch_add(1, Ordering::Relaxed);
                        announce_queue_full.fetch_add(1, Ordering::Relaxed);
                        let _ = store::inc_metric("announces_dropped", 1);
                        let _ = store::inc_metric("announces_queue_full", 1);
                        events::emit(
                            EventType::AnnounceQueueFull,
                            Some(&record.name),
                            Some(&record.endpoint.to_string()),
                            Some("semaphore full and retry queue full"),
                            Some(announce_max_events),
                        );
                    }
                }
                return;
            }
        };

        let pubkey = announce_wg_pubkey.clone();
        let recon = announce_recon.clone();
        let record = record.clone();
        let mp = announce_max_peers;
        let ka = announce_keepalive;
        let plr = announce_peer_limit.clone();
        let sf = announce_store_failures.clone();
        let max_ev = announce_max_events;
        let gossip_enc = announce_enc_key;
        let gossip_pp = announce_peering_port;
        let gossip_tls = announce_tls_client.clone();
        let gossip_source = announce_my_record.clone();
        let gossip_wave_cfg = announce_wave_cfg.clone();
        tokio::spawn(async move {
            let _permit = permit; // held until task completes

            // Check store peer count + existence in a single read transaction (fail closed: skip on error).
            // See on_accepted handler for rationale on store as source of truth.
            let (current_count, exists) = match store::peer_count_and_exists(&record.wg_public_key)
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "on_announce: failed to read peer count, skipping upsert");
                    sf.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            let threshold = mp * 4 / 5; // 80%
            if current_count >= threshold && current_count < mp {
                warn!(
                    current = current_count,
                    max = mp,
                    "peer count approaching limit ({}%)",
                    current_count * 100 / mp
                );
            }

            // Track whether this is a new peer (not previously known) for gossip forwarding.
            let is_new_peer = !exists;

            // Add to WG (bounded)
            match wg::upsert_peer_bounded(&pubkey, &record, mp, current_count, exists, ka) {
                Err(e) => {
                    if matches!(e, wg::WgError::PeerLimitExceeded(_, _)) {
                        plr.fetch_add(1, Ordering::Relaxed);
                        let _ = store::inc_metric("peer_limit_reached", 1);
                        events::emit(
                            EventType::PeerLimitReached,
                            Some(&sanitize(&record.name)),
                            Some(&record.endpoint.to_string()),
                            Some(&format!("max_peers={mp}")),
                            Some(max_ev),
                        );
                    }
                    warn!(peer = %sanitize(&record.name), endpoint = %record.endpoint, error = %e, "failed to upsert announced peer");
                }
                Ok(()) => {
                    recon.fetch_add(1, Ordering::Relaxed);
                    debug!(peer = %sanitize(&record.name), endpoint = %record.endpoint, "peer upserted via announce");
                }
            }
            // Save to store (bounded)
            match store::upsert_peer_bounded(&record, mp) {
                Ok(false) => {
                    warn!(peer = %sanitize(&record.name), max = mp, "peer limit reached, not persisting announced peer");
                    plr.fetch_add(1, Ordering::Relaxed);
                    let _ = store::inc_metric("peer_limit_reached", 1);
                    events::emit(
                        EventType::PeerLimitReached,
                        Some(&sanitize(&record.name)),
                        Some(&record.endpoint.to_string()),
                        Some(&format!("store max_peers={mp}")),
                        Some(max_ev),
                    );
                }
                Err(e) => {
                    warn!(peer = %sanitize(&record.name), error = %e, "failed to persist announced peer");
                }
                Ok(true) => {}
            }

            // Re-gossip: forward to a random subset of our own peers if this
            // is a genuinely new peer we hadn't seen before. The replay guard
            // on the receiving side will drop duplicates, preventing infinite
            // forwarding loops.
            if is_new_peer {
                let known = match store::get_peers() {
                    Ok(peers) => peers,
                    Err(e) => {
                        warn!(error = %e, "gossip forward: failed to load peers");
                        return;
                    }
                };
                let (_ok, _failed) = peering::announce_peer_in_waves(
                    &record,
                    &gossip_source,
                    &known,
                    &gossip_enc,
                    gossip_pp,
                    Some(gossip_tls),
                    &gossip_wave_cfg,
                )
                .await;
            }
        });
    });

    // on_secret_rotation callback: when a SecretRotation message arrives from
    // a peer, apply the new secret locally — re-derive encryption keys, update
    // the store, and rebuild TLS config so subsequent announces use the new key.
    let rotation_shared_secret = shared_mesh_secret.clone();
    let rotation_shared_tls = shared_tls_client.clone();
    let rotation_wg_pubkey = wg_pubkey.clone();
    let rotation_max_events = tuning.max_events;
    let on_secret_rotation: peering::OnSecretRotation = Arc::new(move |new_secret_str| {
        let shared_secret = rotation_shared_secret.clone();
        let shared_tls = rotation_shared_tls.clone();
        let wg_pub = rotation_wg_pubkey.clone();
        let max_ev = rotation_max_events;
        let secret_str = new_secret_str;
        tokio::spawn(async move {
            let new_secret: MeshSecret = match secret_str.parse() {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "on_secret_rotation: failed to parse new secret");
                    return;
                }
            };

            // Normalize to V1 derivation (same as daemon startup).
            let new_secret = MeshSecret::from_bytes(*new_secret.as_bytes());
            let new_prefix = derive_prefix_from_secret(&new_secret);
            let new_ipv6 = addressing::derive_node_address(&new_prefix, wg_pub.as_bytes());

            // Update persisted state.
            match store::load() {
                Ok(mut state) => {
                    state.mesh_secret = secret_str.clone();
                    state.mesh_prefix = new_prefix;
                    state.mesh_ipv6 = new_ipv6;
                    if let Err(e) = store::save(&state) {
                        error!(error = %e, "on_secret_rotation: failed to save state");
                        return;
                    }
                }
                Err(e) => {
                    error!(error = %e, "on_secret_rotation: failed to load state");
                    return;
                }
            }

            // Rebuild TLS client config from new secret.
            let mesh_secret_bytes: [u8; 32] = *new_secret.as_bytes();
            match peering::build_tls_client_config(&mesh_secret_bytes) {
                Ok(new_tls) => {
                    *shared_tls.write().await = new_tls;
                }
                Err(e) => {
                    error!(error = %e, "on_secret_rotation: failed to rebuild TLS config");
                }
            }

            // Swap in the new mesh secret (changes encryption_key() for future announces).
            *shared_secret.write().await = new_secret;

            audit_log::emit(AuditEventType::SecretRotated, None, None, None);
            events::emit(
                EventType::SecretRotated,
                None,
                None,
                Some("received rotation from peer"),
                Some(max_ev),
            );
            info!("secret rotation applied from peer broadcast");
        });
    });

    // Peering listener
    let listener_state = peering_state.clone();
    let mut peering_task = tokio::spawn(async move {
        if let Err(e) = listener_state
            .run_listener(
                peering_port,
                Some(enc_key),
                on_announce,
                on_accepted,
                Some(tls_server_config),
                Some(on_secret_rotation),
            )
            .await
        {
            warn!("peering listener error: {e}");
        }
    });

    // Shutdown coordination: all background loops listen on this token and
    // exit promptly when it is cancelled so we can await their JoinHandles.
    let shutdown = CancellationToken::new();

    // Background drain task: processes queued announces when semaphore permits become available.
    let drain_sem = announce_semaphore.clone();
    let drain_wg_pubkey = wg_pubkey.clone();
    let drain_recon = metrics_reconciliations.clone();
    let drain_max_peers = max_peers;
    let drain_keepalive = keepalive_interval;
    let drain_peer_limit = metrics_peer_limit_reached.clone();
    let drain_store_failures = metrics_store_failures.clone();
    let drain_max_events = tuning.max_events;
    let drain_queue_rx = announce_queue_rx;
    let shutdown_drain = shutdown.clone();
    let mut drain_task = tokio::spawn(async move {
        loop {
            // Wait for a record from the queue, or shutdown.
            let record = {
                let mut rx = drain_queue_rx.lock().await;
                tokio::select! {
                    _ = shutdown_drain.cancelled() => break,
                    msg = rx.recv() => match msg {
                        Some(r) => r,
                        None => break, // channel closed
                    },
                }
            };

            // Wait for a semaphore permit (blocking — this is the retry).
            let permit = tokio::select! {
                _ = shutdown_drain.cancelled() => break,
                res = drain_sem.clone().acquire_owned() => match res {
                    Ok(p) => p,
                    Err(_) => break, // semaphore closed
                },
            };

            let pubkey = drain_wg_pubkey.clone();
            let recon = drain_recon.clone();
            let mp = drain_max_peers;
            let ka = drain_keepalive;
            let plr = drain_peer_limit.clone();
            let sf = drain_store_failures.clone();
            let max_ev = drain_max_events;

            tokio::spawn(async move {
                let _permit = permit;
                debug!(peer = %sanitize(&record.name), "processing queued announce (retry)");

                let (current_count, exists) =
                    match store::peer_count_and_exists(&record.wg_public_key) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, "drain: failed to read peer count, skipping upsert");
                            sf.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                    };

                match wg::upsert_peer_bounded(&pubkey, &record, mp, current_count, exists, ka) {
                    Err(e) => {
                        if matches!(e, wg::WgError::PeerLimitExceeded(_, _)) {
                            plr.fetch_add(1, Ordering::Relaxed);
                            let _ = store::inc_metric("peer_limit_reached", 1);
                            events::emit(
                                EventType::PeerLimitReached,
                                Some(&sanitize(&record.name)),
                                Some(&record.endpoint.to_string()),
                                Some(&format!("max_peers={mp}")),
                                Some(max_ev),
                            );
                        }
                        warn!(peer = %sanitize(&record.name), endpoint = %record.endpoint, error = %e, "failed to upsert queued peer");
                    }
                    Ok(()) => {
                        recon.fetch_add(1, Ordering::Relaxed);
                        debug!(peer = %sanitize(&record.name), endpoint = %record.endpoint, "queued peer upserted via announce drain");
                    }
                }
                match store::upsert_peer_bounded(&record, mp) {
                    Ok(false) => {
                        warn!(peer = %sanitize(&record.name), max = mp, "peer limit reached, not persisting queued peer");
                        plr.fetch_add(1, Ordering::Relaxed);
                        let _ = store::inc_metric("peer_limit_reached", 1);
                    }
                    Err(e) => {
                        warn!(peer = %sanitize(&record.name), error = %e, "failed to persist queued peer");
                    }
                    Ok(true) => {}
                }
            });
        }
    });

    // Notify systemd that the daemon is ready (Type=notify).
    // At this point the WireGuard interface is up, the control socket is
    // listening, and the peering listener is accepting connections.
    sd_watchdog::notify_ready();
    sd_watchdog::notify_status("Mesh daemon running");

    // Persist metrics (atomic — no load+modify+save)
    let persist_recv = metrics_received.clone();
    let persist_recon = metrics_reconciliations.clone();
    let persist_unreach = metrics_unreachable.clone();
    let persist_dropped = metrics_announces_dropped.clone();
    let persist_announces_queued = metrics_announces_queued.clone();
    let persist_announces_queue_full = metrics_announces_queue_full.clone();
    let persist_peer_limit = metrics_peer_limit_reached.clone();
    let persist_health_failures = metrics_health_check_failures.clone();
    let persist_reconcile_failures = metrics_reconcile_failures.clone();
    let persist_store_failures = metrics_store_failures.clone();
    let persist_peering_state = peering_state.clone();
    let shutdown_persist = shutdown.clone();
    let gc_threshold_secs = tuning.gc_removed_threshold.as_secs();
    let persist = async move {
        let mut interval = tokio::time::interval(tuning.persist_interval);
        loop {
            tokio::select! {
                _ = shutdown_persist.cancelled() => break,
                _ = interval.tick() => {}
            }
            let _ = store::set_metric("peers_discovered", persist_recv.load(Ordering::Relaxed));
            let _ = store::set_metric("wg_reconciliations", persist_recon.load(Ordering::Relaxed));
            let _ = store::set_metric(
                "peers_marked_unreachable",
                persist_unreach.load(Ordering::Relaxed),
            );
            let _ = store::set_metric("daemon_started_at", daemon_started);
            let _ = store::set_metric(
                "connections_rejected",
                persist_peering_state.connections_rejected(),
            );
            let _ = store::set_metric(
                "connections_active",
                persist_peering_state.connections_active(),
            );
            let _ = store::set_metric("announces_dropped", persist_dropped.load(Ordering::Relaxed));
            let _ = store::set_metric(
                "announces_queued",
                persist_announces_queued.load(Ordering::Relaxed),
            );
            let _ = store::set_metric(
                "announces_queue_full",
                persist_announces_queue_full.load(Ordering::Relaxed),
            );
            let _ = store::set_metric(
                "peer_limit_reached",
                persist_peer_limit.load(Ordering::Relaxed),
            );
            if let Err(e) = store::set_metric(
                "health_check_failures",
                persist_health_failures.load(Ordering::Relaxed),
            ) {
                debug!(error = %e, "failed to persist health_check_failures metric");
            }
            if let Err(e) = store::set_metric(
                "reconcile_failures",
                persist_reconcile_failures.load(Ordering::Relaxed),
            ) {
                debug!(error = %e, "failed to persist reconcile_failures metric");
            }
            if let Err(e) = store::set_metric(
                "store_failures",
                persist_store_failures.load(Ordering::Relaxed),
            ) {
                debug!(error = %e, "failed to persist store_failures metric");
            }

            // Garbage-collect peers that have been Removed for longer than
            // the configured threshold (default 24 h).
            match store::gc_removed_peers(gc_threshold_secs) {
                Ok(n) if n > 0 => {
                    info!(count = n, "garbage-collected removed peers");
                }
                Err(e) => {
                    debug!(error = %e, "failed to gc removed peers");
                }
                _ => {}
            }

            // Flush JSON export so state.json stays in sync with redb
            let _ = store::flush_json();
        }
    };

    // Health check: unreachable detection + recovery + last_seen update
    let health_policy = tuning.health_policy.clone();
    let health_my_topology = my_record.topology.clone();
    let health_counter = metrics_unreachable.clone();
    let health_recon = metrics_reconciliations.clone();
    let health_max_events = tuning.max_events;
    let health_failures = metrics_health_check_failures.clone();
    let shutdown_health = shutdown.clone();
    let health_check = async move {
        let mut interval = tokio::time::interval(tuning.health_check_interval);
        loop {
            tokio::select! {
                _ = shutdown_health.cancelled() => break,
                _ = interval.tick() => {}
            }

            // Get WireGuard handshake data for all peers
            let wg_peers = match wg::interface_summary() {
                Ok(s) => s.peers,
                Err(e) => {
                    warn!(error = %e, "health check: WireGuard interface unavailable");
                    health_failures.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            let mut peers = match store::get_peers() {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "health check: failed to load peers from store");
                    health_failures.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            let current = now();
            let mut changed = false;

            for peer in peers.iter_mut() {
                // Find matching WG peer by public key
                let wg_peer = wg_peers.iter().find(|p| p.public_key == peer.wg_public_key);

                let wg_handshake_epoch = wg_peer.and_then(|wp| {
                    wp.last_handshake
                        .map(|ht| ht.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                });

                let peer_timeout =
                    timeout_for_peer(&health_my_topology, &peer.topology, &health_policy);

                let old_status = peer.status;
                let peer_changed =
                    evaluate_peer_health(peer, wg_handshake_epoch, current, peer_timeout);

                if peer_changed {
                    changed = true;

                    if old_status == PeerStatus::Unreachable && peer.status == PeerStatus::Active {
                        info!(peer = %sanitize(&peer.name), last_seen = peer.last_seen, "peer recovered, marking active");
                        events::emit(
                            EventType::PeerRecovered,
                            Some(&sanitize(&peer.name)),
                            Some(&peer.endpoint.to_string()),
                            Some("handshake resumed"),
                            Some(health_max_events),
                        );
                    }
                    if old_status == PeerStatus::Active && peer.status == PeerStatus::Unreachable {
                        info!(peer = %sanitize(&peer.name), last_seen = peer.last_seen, timeout_secs = peer_timeout, "marking peer as unreachable");
                        health_counter.fetch_add(1, Ordering::Relaxed);
                        events::emit(
                            EventType::PeerUnreachable,
                            Some(&sanitize(&peer.name)),
                            Some(&peer.endpoint.to_string()),
                            Some(&format!("no handshake for {peer_timeout}s")),
                            Some(health_max_events),
                        );
                    }
                }

                // Never remove a peer from WireGuard that has a recent handshake.
                // The health check only transitions between Active/Unreachable;
                // it never sets Removed or calls wg remove. Peer removal is only
                // done through explicit leave/remove operations.
            }

            // Persist changes atomically
            if changed {
                for peer in &peers {
                    if let Err(e) = store::upsert_peer(peer) {
                        warn!(peer = %sanitize(&peer.name), error = %e, "health check: failed to persist peer state");
                        health_failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            // ── Zone health aggregation ──────────────────────────────
            {
                use crate::events::ZoneHealthStatus;
                use crate::topology::TopologyView;

                let view = TopologyView::from_peers(&peers);
                let mut all_zones: Vec<&syfrah_core::mesh::Zone> = view
                    .regions()
                    .iter()
                    .flat_map(|r| view.zones_in_region(r))
                    .collect();
                all_zones.sort_by_key(|z| z.as_str());
                all_zones.dedup_by_key(|z| z.as_str());

                for zone in all_zones {
                    let zone_peers = view.peers_in_zone(zone);
                    let total = zone_peers.len();
                    let active = view.active_count_in_zone(zone);
                    let new_status = ZoneHealthStatus::from_counts(active, total);

                    let prev_status = store::get_zone_health(zone.as_str()).unwrap_or(None);

                    // Persist the new status
                    if let Err(e) = store::set_zone_health(zone.as_str(), new_status) {
                        warn!(zone = %zone.as_str(), error = %e, "failed to persist zone health");
                    }

                    // Emit event on transition (skip initial Healthy → Healthy)
                    if let Some(prev) = prev_status {
                        if prev != new_status {
                            if let Some(event_type) = new_status.transition_event() {
                                info!(
                                    zone = %zone.as_str(),
                                    from = %prev,
                                    to = %new_status,
                                    active,
                                    total,
                                    "zone health transition"
                                );
                                events::emit(
                                    event_type,
                                    None,
                                    None,
                                    Some(&format!(
                                        "zone={} active={}/{} status={}",
                                        zone.as_str(),
                                        active,
                                        total,
                                        new_status,
                                    )),
                                    Some(health_max_events),
                                );
                            }
                        }
                    }
                }
            }

            events::emit(
                EventType::HealthCheckRun,
                None,
                None,
                Some(&format!("peers_checked={}", peers.len())),
                Some(health_max_events),
            );

            // Ping systemd watchdog after each successful health check cycle.
            // With WatchdogSec=60 and the default health_check_interval of 30s,
            // this keeps the watchdog fed as long as the health loop is running.
            sd_watchdog::notify_watchdog();
        }
    };

    // Reconciliation loop: compare stored peers with WireGuard config
    let reconcile_wg_pubkey = wg_pubkey.clone();
    let reconcile_recon = health_recon;
    let reconcile_max_events = tuning.max_events;
    let reconcile_keepalive = keepalive_interval;
    let reconcile_failures = metrics_reconcile_failures.clone();
    let shutdown_reconcile = shutdown.clone();
    let reconcile = async move {
        let mut interval = tokio::time::interval(tuning.reconcile_interval);
        loop {
            tokio::select! {
                _ = shutdown_reconcile.cancelled() => break,
                _ = interval.tick() => {}
            }

            let stored_peers = match store::get_peers() {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "reconciliation: failed to load peers from store");
                    reconcile_failures.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            // Fix 0.0.0.0 endpoints: if a stored peer has an unspecified endpoint
            // but WireGuard has learned a real endpoint via roaming, update the
            // store so the correct endpoint is propagated (issue #285).
            if let Ok(summary) = wg::interface_summary() {
                for peer in &stored_peers {
                    if peer.endpoint.ip().is_unspecified() {
                        if let Some(wg_peer) = summary
                            .peers
                            .iter()
                            .find(|wp| wp.public_key == peer.wg_public_key)
                        {
                            if let Some(real_endpoint) = wg_peer.endpoint {
                                if !real_endpoint.ip().is_unspecified()
                                    && !real_endpoint.ip().is_loopback()
                                {
                                    info!(
                                        peer = %sanitize(&peer.name),
                                        old_endpoint = %peer.endpoint,
                                        new_endpoint = %real_endpoint,
                                        "correcting 0.0.0.0 endpoint from WG roaming data"
                                    );
                                    let _ = store::update_peer_endpoint(
                                        &peer.wg_public_key,
                                        real_endpoint,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            // Diff-based reconciliation: only touch peers that actually changed.
            // This avoids tearing down existing WireGuard sessions.
            // sync_peers handles diff, add/update, removal + route cleanup.
            match wg::sync_peers(&reconcile_wg_pubkey, &stored_peers, reconcile_keepalive) {
                Ok(0) => {
                    debug!("reconciliation: no diff, skipping");
                }
                Ok(n) => {
                    reconcile_recon.fetch_add(n as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    warn!(error = %e, "reconciliation: sync_peers failed");
                    reconcile_failures.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }

            events::emit(
                EventType::ReconciliationRun,
                None,
                None,
                Some(&format!("stored_peers={}", stored_peers.len())),
                Some(reconcile_max_events),
            );
        }
    };

    // Periodic self-announce (anti-entropy): re-announce our own record to
    // ALL known peers so that any announcements lost during initial join
    // propagation are eventually recovered. Without this, a failed announce
    // between two peers can leave them permanently unaware of each other.
    //
    // Unlike event-driven gossip (which uses fanout to limit traffic), the
    // self-announce loop broadcasts to every peer because it runs infrequently
    // and must guarantee convergence within a bounded number of rounds.
    let self_announce_record = my_record.clone();
    let self_announce_enc = enc_key;
    let self_announce_port = peering_port;
    let self_announce_tls = tls_client_config.clone();
    let self_announce_interval = tuning.self_announce_interval;
    let shutdown_self_announce = shutdown.clone();
    let self_announce = async move {
        // Stagger initial delay to avoid thundering herd at mesh startup.
        tokio::select! {
            _ = shutdown_self_announce.cancelled() => return,
            _ = tokio::time::sleep(self_announce_interval) => {}
        }
        let mut interval = tokio::time::interval(self_announce_interval);
        loop {
            tokio::select! {
                _ = shutdown_self_announce.cancelled() => break,
                _ = interval.tick() => {}
            }
            let known = match store::get_peers() {
                Ok(p) => p,
                Err(e) => {
                    debug!(error = %e, "self-announce: failed to load peers, skipping round");
                    continue;
                }
            };
            if known.is_empty() {
                continue;
            }
            // Broadcast to ALL peers (not gossip subset) for reliable convergence.
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            for peer in &known {
                if peer.wg_public_key == self_announce_record.wg_public_key {
                    continue; // skip self
                }
                if let Err(e) = peering::announce_peer(
                    peer.endpoint,
                    self_announce_port,
                    &self_announce_record,
                    &self_announce_enc,
                    Some(self_announce_tls.clone()),
                )
                .await
                {
                    debug!(
                        target_peer = %sanitize(&peer.name),
                        error = %e,
                        "self-announce to peer failed"
                    );
                    failed += 1;
                } else {
                    succeeded += 1;
                }
            }
            if failed > 0 {
                debug!(
                    succeeded = succeeded,
                    failed = failed,
                    "self-announce round completed with failures"
                );
            } else if succeeded > 0 {
                debug!(succeeded = succeeded, "self-announce round completed");
            }
        }
    };

    // Listen for both SIGINT (Ctrl+C) and SIGTERM so the daemon shuts down
    // gracefully in either case. Without SIGTERM handling, `run_leave` sends
    // SIGTERM but the daemon dies immediately without cleanup.
    #[cfg(unix)]
    let terminate = async {
        let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    // SIGHUP handler: reload config without restart.
    let sighup_max_events = tuning.max_events;
    let shutdown_sighup = shutdown.clone();
    #[cfg(unix)]
    let sighup_reload = async move {
        let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .expect("failed to register SIGHUP handler");
        loop {
            tokio::select! {
                _ = shutdown_sighup.cancelled() => break,
                _ = sig.recv() => {
                    info!("received SIGHUP, reloading configuration");
                    handle_reload(sighup_max_events, None);
                }
            }
        }
    };
    #[cfg(not(unix))]
    let sighup_reload = std::future::pending::<()>();

    // HTTP API server (serves /metrics for Prometheus and topology endpoints).
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::watch::channel(false);
    let api_task = tokio::spawn(http_api::serve(api_config, api_shutdown_rx));

    // gRPC-compatible API server (external fabric API on configurable port).
    // When gateway mode is enabled, the gateway TLS server replaces the plain
    // gRPC server for external traffic.
    let (grpc_shutdown_tx, grpc_shutdown_rx) = tokio::sync::watch::channel(false);
    let grpc_task = if gateway_config.enabled {
        // Gateway mode: start TLS-protected external API.
        let (gw_shutdown_tx, gw_shutdown_rx) = tokio::sync::watch::channel(false);
        // Re-purpose grpc_shutdown_tx to also shut down the gateway.
        let gw_handler = grpc_handler.clone();
        let gw_config = gateway_config.clone();
        let gw_task = tokio::spawn(crate::grpc_api::serve_gateway_tls(
            gw_config,
            gw_handler,
            gw_shutdown_rx,
        ));
        // Also start the plain gRPC server if it was explicitly enabled (for
        // internal mesh-only access).
        let plain_task = tokio::spawn(crate::grpc_api::serve(
            grpc_config,
            grpc_handler,
            grpc_shutdown_rx,
        ));
        // We need both shutdown senders alive. Store the gateway one alongside.
        // For simplicity we just leak the extra sender into a spawn that waits
        // for both tasks.
        tokio::spawn(async move {
            tokio::select! {
                _ = gw_task => {}
                _ = plain_task => {}
            }
            let _ = gw_shutdown_tx.send(true);
        })
    } else {
        // No gateway: only start the plain gRPC server (disabled by default).
        tokio::spawn(crate::grpc_api::serve(
            grpc_config,
            grpc_handler,
            grpc_shutdown_rx,
        ))
    };

    // SIGUSR1 handler: flush state and re-exec for zero-downtime update.
    #[cfg(unix)]
    let sigusr1_reexec = async {
        let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
            .expect("failed to register SIGUSR1 handler");
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let sigusr1_reexec = std::future::pending::<()>();

    // Wait for a shutdown signal (SIGINT, SIGTERM, or SIGUSR1) or an unexpected task exit.
    let mut reexec_requested = false;
    tokio::select! {
        _ = &mut control_task => {
            warn!("control_task exited unexpectedly");
        }
        _ = &mut peering_task => {
            warn!("peering_task exited unexpectedly");
        }
        _ = &mut drain_task => {
            warn!("drain_task exited unexpectedly");
        }
        _ = persist => {}
        _ = health_check => {}
        _ = reconcile => {}
        _ = self_announce => {}
        _ = sighup_reload => {}
        _ = api_task => {}
        _ = grpc_task => {}
        _ = forge_task => {}
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, shutting down");
        }
        _ = terminate => {
            info!("received SIGTERM, shutting down");
        }
        _ = sigusr1_reexec => {
            info!("received SIGUSR1, scheduling re-exec after in-flight operations drain");
            reexec_requested = true;
        }
    }

    // If SIGUSR1 was received, give in-flight control socket operations (e.g.
    // long image pulls) a grace period to finish before we proceed with re-exec.
    // We wait up to 5 seconds for the control task to become idle rather than
    // aborting it immediately.
    if reexec_requested {
        info!("waiting up to 5s for in-flight operations to complete...");
        let drain_grace = tokio::time::Duration::from_secs(5);
        let _ = tokio::time::timeout(drain_grace, async {
            // Yield to the runtime so in-flight control socket responses can
            // complete naturally. We don't cancel anything here — the tasks
            // continue running and will finish their current request/response
            // cycle during this window.
            tokio::task::yield_now().await;
            tokio::time::sleep(drain_grace).await;
        })
        .await;
        info!("grace period elapsed, proceeding with re-exec");
    }

    // Signal all cancellation-aware loops to stop.
    shutdown.cancel();

    // Signal HTTP API server to shut down gracefully.
    let _ = api_shutdown_tx.send(true);

    // Signal gRPC API server to shut down gracefully.
    let _ = grpc_shutdown_tx.send(true);

    // Signal Forge HTTP API server to shut down gracefully.
    let _ = forge_shutdown_tx.send(true);

    // Tell systemd we are shutting down gracefully.
    sd_watchdog::notify_stopping();

    // Abort control and peering tasks (they block on accept loops that
    // don't have a built-in cancellation path) and await completion so
    // any in-flight work finishes or is cancelled cleanly.
    control_task.abort();
    peering_task.abort();

    // Await spawned tasks so in-flight work completes before we tear down
    // the WireGuard interface. Use a timeout to avoid hanging forever.
    let grace = tokio::time::Duration::from_secs(5);
    let _ = tokio::time::timeout(grace, async {
        // drain_task listens on the CancellationToken and will exit on its own.
        let _ = drain_task.await;
        let _ = control_task.await;
        let _ = peering_task.await;
    })
    .await;

    info!("all async tasks completed (or timed out), proceeding with cleanup");

    // Flush any debounced JSON state so the on-disk export is up-to-date.
    let _ = store::flush_json();
    let _ = std::fs::remove_file(store::control_socket_path());

    if reexec_requested {
        // Re-exec: replace this process with the (potentially updated) binary.
        // WireGuard interface stays up (kernel-managed). PID file is kept so
        // the new process can overwrite it. We skip teardown_interface and
        // remove_pid intentionally.
        info!("flushed state, re-executing daemon...");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let exe = std::env::current_exe().expect("failed to determine current executable");
            let args: Vec<String> = std::env::args().collect();
            // exec() replaces this process — it does not return on success.
            let err = std::process::Command::new(&exe).args(&args[1..]).exec();
            // If we get here, exec() failed. Do NOT fall through to
            // teardown_interface/remove_pid — that would destroy the WireGuard
            // interface and PID file, leaving the daemon unrecoverable.
            // Instead, exit immediately so systemd can restart us with the
            // existing interface and PID file intact.
            error!("re-exec failed: {err} — exiting for systemd restart");
            std::process::exit(1);
        }
        #[cfg(not(unix))]
        {
            error!("re-exec is not supported on this platform — exiting for systemd restart");
            std::process::exit(1);
        }
    }

    wg::teardown_interface()?;
    store::remove_pid();
    events::emit(
        EventType::DaemonStopped,
        None,
        None,
        None,
        Some(tuning.max_events),
    );
    audit_log::emit(AuditEventType::DaemonStopped, None, None, None);
    info!("daemon stopped");
    Ok(())
}

/// Control handler for the daemon.
struct DaemonFabricHandler {
    peering_state: Arc<PeeringState>,
    mesh_secret: Arc<tokio::sync::RwLock<MeshSecret>>,
    my_record: PeerRecord,
    wg_pubkey: Key,
    peering_port: u16,
    on_accepted: peering::OnAccepted,
    tls_client_config: Arc<tokio::sync::RwLock<Arc<rustls::ClientConfig>>>,
    max_events: u64,
    max_peers: usize,
}

#[async_trait::async_trait]
impl FabricHandler for DaemonFabricHandler {
    async fn handle(&self, req: FabricRequest, caller_uid: Option<u32>) -> FabricResponse {
        match req {
            FabricRequest::PeeringStart { port: _, pin } => {
                if let Some(pin_val) = pin {
                    let state = match store::load() {
                        Ok(s) => s,
                        Err(e) => {
                            return FabricResponse::Error {
                                message: format!("{e}"),
                            }
                        }
                    };
                    self.peering_state
                        .set_auto_accept(Some(AutoAcceptConfig {
                            pin: pin_val,
                            mesh_name: state.mesh_name,
                            mesh_secret_str: state.mesh_secret,
                            mesh_prefix: state.mesh_prefix,
                            my_record: self.my_record.clone(),
                            wg_pubkey: self.wg_pubkey.clone(),
                            encryption_key: self.mesh_secret.read().await.encryption_key(),
                            peering_port: self.peering_port,
                            max_peers: self.max_peers,
                        }))
                        .await;
                }
                self.peering_state.set_active(true).await;
                audit_log::emit_with_uid(
                    AuditEventType::PeeringStarted,
                    None,
                    None,
                    None,
                    caller_uid,
                );
                FabricResponse::Ok
            }
            FabricRequest::PeeringStop => {
                self.peering_state.set_active(false).await;
                self.peering_state.set_auto_accept(None).await;
                audit_log::emit_with_uid(
                    AuditEventType::PeeringStopped,
                    None,
                    None,
                    None,
                    caller_uid,
                );
                FabricResponse::Ok
            }
            FabricRequest::PeeringList => {
                let requests = self.peering_state.list_pending().await;
                FabricResponse::PeeringList { requests }
            }
            FabricRequest::PeeringAccept { request_id } => {
                let state = match store::load() {
                    Ok(s) => s,
                    Err(e) => {
                        return FabricResponse::Error {
                            message: format!("{e}"),
                        }
                    }
                };

                let mut all_peers = state.peers.clone();
                all_peers.push(self.my_record.clone());

                let response = syfrah_core::mesh::JoinResponse {
                    accepted: true,
                    mesh_name: Some(state.mesh_name.clone()),
                    mesh_secret: Some(state.mesh_secret.clone()),
                    mesh_prefix: Some(state.mesh_prefix),
                    peers: all_peers,
                    reason: None,
                    approved_by: Some("manual".into()),
                };

                match self.peering_state.accept(&request_id, response).await {
                    Ok(info) => {
                        let new_wg_pub = match Key::from_base64(&info.wg_public_key) {
                            Ok(k) => k,
                            Err(_) => {
                                return FabricResponse::Error {
                                    message: "invalid WG key".into(),
                                }
                            }
                        };
                        let new_mesh_ipv6 = addressing::derive_node_address(
                            &state.mesh_prefix,
                            new_wg_pub.as_bytes(),
                        );
                        // Use the joiner's region/zone from the request.
                        // If zone was not provided, auto-generate one
                        // using the current peer list.
                        let (region, zone) = resolve_region_zone(
                            info.region.as_deref(),
                            info.zone.as_deref(),
                            &state.peers,
                        );
                        let topology =
                            syfrah_core::mesh::Topology::from_strings(Some(&region), Some(&zone));
                        let new_record = PeerRecord {
                            name: info.node_name.clone(),
                            wg_public_key: info.wg_public_key,
                            endpoint: info.endpoint,
                            mesh_ipv6: new_mesh_ipv6,
                            last_seen: now(),
                            status: PeerStatus::Active,
                            region: Some(region),
                            zone: Some(zone),
                            topology,
                        };
                        events::emit(
                            EventType::JoinManuallyAccepted,
                            Some(&sanitize(&info.node_name)),
                            Some(&info.endpoint.to_string()),
                            None,
                            Some(self.max_events),
                        );
                        audit_log::emit_with_uid(
                            AuditEventType::PeerJoinAccepted,
                            Some(&sanitize(&info.node_name)),
                            Some(&info.endpoint.to_string()),
                            Some("approved_by=manual"),
                            caller_uid,
                        );
                        (self.on_accepted)(new_record);
                        FabricResponse::PeeringAccepted {
                            peer_name: info.node_name,
                        }
                    }
                    Err(e) => FabricResponse::Error {
                        message: e.to_string(),
                    },
                }
            }
            FabricRequest::PeeringReject { request_id, reason } => {
                match self.peering_state.reject(&request_id, reason.clone()).await {
                    Ok(()) => {
                        events::emit(
                            EventType::JoinRejected,
                            None,
                            None,
                            reason.as_deref(),
                            Some(self.max_events),
                        );
                        audit_log::emit_with_uid(
                            AuditEventType::PeerJoinRejected,
                            None,
                            None,
                            reason.as_deref(),
                            caller_uid,
                        );
                        FabricResponse::Ok
                    }
                    Err(e) => FabricResponse::Error {
                        message: e.to_string(),
                    },
                }
            }
            FabricRequest::Reload => handle_reload(self.max_events, caller_uid),
            FabricRequest::RemovePeer { name_or_key } => {
                let state = match store::load() {
                    Ok(s) => s,
                    Err(e) => {
                        return FabricResponse::Error {
                            message: format!("{e}"),
                        }
                    }
                };

                // Prevent removing self
                if name_or_key == state.node_name || name_or_key == state.wg_public_key {
                    return FabricResponse::Error {
                        message: "Cannot remove self. Use 'syfrah fabric leave' instead.".into(),
                    };
                }

                // Mark peer as Removed in the store
                match store::remove_peer(&name_or_key) {
                    Ok(Some(removed_peer)) => {
                        // Remove from WireGuard and clean up route
                        if let Ok(self_key) = Key::from_base64(&state.wg_public_key) {
                            let tuning = config::load_tuning().unwrap_or_default();
                            let _ = wg::sync_peers(
                                &self_key,
                                &store::get_peers().unwrap_or_default(),
                                tuning.keepalive_interval,
                            );
                        }

                        let peer_name = sanitize(&removed_peer.name);

                        events::emit(
                            EventType::PeerRemoved,
                            Some(&peer_name),
                            Some(&removed_peer.endpoint.to_string()),
                            None,
                            Some(self.max_events),
                        );

                        // Announce removal to other peers
                        let peers = store::get_peers().unwrap_or_default();
                        let active_peers: Vec<_> = peers
                            .iter()
                            .filter(|p| p.status != PeerStatus::Removed)
                            .cloned()
                            .collect();
                        let encryption_key = self.mesh_secret.read().await.encryption_key();
                        let tls_cfg = self.tls_client_config.read().await.clone();
                        let (announced, _failed) = peering::announce_peer_to_mesh(
                            &removed_peer,
                            &active_peers,
                            &encryption_key,
                            self.peering_port,
                            Some(tls_cfg),
                        )
                        .await;

                        FabricResponse::PeerRemoved {
                            peer_name: removed_peer.name.clone(),
                            announced_to: announced,
                        }
                    }
                    Ok(None) => FabricResponse::Error {
                        message: format!(
                            "No peer named '{}'. Run 'syfrah fabric peers' to list peers.",
                            name_or_key
                        ),
                    },
                    Err(e) => FabricResponse::Error {
                        message: format!("Failed to remove peer: {e}"),
                    },
                }
            }
            FabricRequest::UpdatePeerEndpoint {
                name_or_key,
                endpoint,
            } => {
                let state = match store::load() {
                    Ok(s) => s,
                    Err(e) => {
                        return FabricResponse::Error {
                            message: format!("{e}"),
                        }
                    }
                };

                match store::update_peer_endpoint(&name_or_key, endpoint) {
                    Ok(Some((old_endpoint, updated_peer))) => {
                        // Apply to WireGuard
                        if let Ok(self_key) = Key::from_base64(&state.wg_public_key) {
                            let tuning = config::load_tuning().unwrap_or_default();
                            let _ = wg::upsert_peer(
                                &self_key,
                                &updated_peer,
                                tuning.keepalive_interval,
                            );
                        }

                        let peer_name = sanitize(&updated_peer.name);

                        events::emit(
                            EventType::PeerUpdated,
                            Some(&peer_name),
                            Some(&format!("{} -> {}", old_endpoint, endpoint)),
                            None,
                            Some(self.max_events),
                        );

                        FabricResponse::PeerEndpointUpdated {
                            peer_name: updated_peer.name.clone(),
                            old_endpoint: old_endpoint.to_string(),
                            new_endpoint: endpoint.to_string(),
                        }
                    }
                    Ok(None) => FabricResponse::Error {
                        message: format!(
                            "No peer named '{}'. Run 'syfrah fabric peers' to list peers.",
                            name_or_key
                        ),
                    },
                    Err(e) => FabricResponse::Error {
                        message: format!("Failed to update peer endpoint: {e}"),
                    },
                }
            }
            FabricRequest::RotateSecret => {
                // 1. Read current secret for encrypting the rotation broadcast.
                let old_secret = self.mesh_secret.read().await.clone();
                let old_enc_key = old_secret.encryption_key();

                // 2. Generate new secret and derive new addressing.
                let new_secret = MeshSecret::generate();
                let new_secret_str = new_secret.to_string();
                // Normalize to V1 (same as daemon startup / peer parsing).
                let new_secret = MeshSecret::from_bytes(*new_secret.as_bytes());
                let new_prefix = derive_prefix_from_secret(&new_secret);
                let new_ipv6 =
                    addressing::derive_node_address(&new_prefix, self.wg_pubkey.as_bytes());

                // 3. Encrypt the new secret string with the OLD key for broadcast.
                let encrypted_secret =
                    match syfrah_core::mesh::encrypt_secret(&new_secret_str, &old_enc_key) {
                        Ok(ct) => ct,
                        Err(e) => {
                            return FabricResponse::Error {
                                message: format!("failed to encrypt new secret: {e}"),
                            }
                        }
                    };

                // 4. Broadcast the rotation to all active peers (using old TLS config).
                let peers = store::get_peers().unwrap_or_default();
                let tls_cfg = self.tls_client_config.read().await.clone();
                let (notified, failed) = peering::broadcast_secret_rotation(
                    &encrypted_secret,
                    &peers,
                    self.peering_port,
                    Some(tls_cfg),
                )
                .await;

                // 5. Update local state with new secret.
                match store::load() {
                    Ok(mut state) => {
                        state.mesh_secret = new_secret_str.clone();
                        state.mesh_prefix = new_prefix;
                        state.mesh_ipv6 = new_ipv6;
                        if let Err(e) = store::save(&state) {
                            return FabricResponse::Error {
                                message: format!("secret broadcast succeeded but save failed: {e}"),
                            };
                        }
                    }
                    Err(e) => {
                        return FabricResponse::Error {
                            message: format!(
                                "secret broadcast succeeded but state load failed: {e}"
                            ),
                        };
                    }
                }

                // 6. Rebuild TLS client config from new secret.
                let new_secret_bytes: [u8; 32] = *new_secret.as_bytes();
                match peering::build_tls_client_config(&new_secret_bytes) {
                    Ok(new_tls) => {
                        *self.tls_client_config.write().await = new_tls;
                    }
                    Err(e) => {
                        warn!(error = %e, "RotateSecret: failed to rebuild TLS config");
                    }
                }

                // 7. Swap in the new mesh secret for future encryption_key() calls.
                *self.mesh_secret.write().await = new_secret;

                audit_log::emit_with_uid(
                    AuditEventType::SecretRotated,
                    None,
                    None,
                    None,
                    caller_uid,
                );
                events::emit(
                    EventType::SecretRotated,
                    None,
                    None,
                    Some(&format!("notified={notified} failed={failed}")),
                    Some(self.max_events),
                );
                info!(
                    notified = notified,
                    failed = failed,
                    "secret rotation completed"
                );

                FabricResponse::SecretRotated {
                    new_secret: new_secret_str,
                    new_ipv6: new_ipv6.to_string(),
                    peers_notified: notified,
                    peers_failed: failed,
                }
            }
        }
    }
}

/// Handle a config reload request: re-read config.toml, diff with current,
/// apply hot-reloadable changes, and report results.
fn handle_reload(max_events: u64, caller_uid: Option<u32>) -> FabricResponse {
    // Dry-run: parse and validate the config file before applying any changes.
    if let Err(e) = config::validate_config_file() {
        warn!("config reload rejected (validation failed): {e}");
        events::emit(
            EventType::ConfigReloadFailed,
            None,
            None,
            Some(&e),
            Some(max_events),
        );
        return FabricResponse::Error {
            message: format!("Config validation failed: {e}. Keeping current configuration."),
        };
    }

    let current = config::load_tuning().unwrap_or_default();
    match config::load_tuning() {
        Ok(new_tuning) => {
            let (changes, skipped) = config::diff_tuning(&current, &new_tuning);

            let change_strs: Vec<String> = changes
                .iter()
                .map(|c| format!("{} {} -> {}", c.name, c.old_value, c.new_value))
                .collect();
            let skip_strs: Vec<String> = skipped
                .iter()
                .map(|c| {
                    format!(
                        "{} {} -> {} (requires restart)",
                        c.name, c.old_value, c.new_value
                    )
                })
                .collect();

            let detail = if change_strs.is_empty() && skip_strs.is_empty() {
                "no changes".to_string()
            } else {
                change_strs
                    .iter()
                    .chain(skip_strs.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            info!("config reloaded: {detail}");
            events::emit(
                EventType::ConfigReloaded,
                None,
                None,
                Some(&detail),
                Some(max_events),
            );
            audit_log::emit_with_uid(
                AuditEventType::ConfigReloaded,
                None,
                None,
                Some(&detail),
                caller_uid,
            );

            FabricResponse::ConfigReloaded {
                changes: change_strs,
                skipped: skip_strs,
            }
        }
        Err(e) => {
            warn!("config reload failed: {e}");
            events::emit(
                EventType::ConfigReloadFailed,
                None,
                None,
                Some(&e),
                Some(max_events),
            );
            FabricResponse::Error {
                message: format!("Config reload failed: {e}. Keeping current configuration."),
            }
        }
    }
}

fn build_record(
    name: &str,
    wg_keypair: &KeyPair,
    endpoint: SocketAddr,
    mesh_ipv6: std::net::Ipv6Addr,
    region: Option<&str>,
    zone: Option<&str>,
) -> PeerRecord {
    use syfrah_core::mesh::Topology;
    let topology = Topology::from_strings(region, zone);
    PeerRecord {
        name: name.to_string(),
        wg_public_key: wg_keypair.public.to_base64(),
        endpoint,
        mesh_ipv6,
        last_seen: now(),
        status: PeerStatus::Active,
        region: region.map(|s| s.to_string()),
        zone: zone.map(|s| s.to_string()),
        topology,
    }
}

fn resolve_endpoint(config: &DaemonConfig) -> SocketAddr {
    config
        .public_endpoint
        .unwrap_or_else(|| SocketAddr::new("0.0.0.0".parse().unwrap(), config.wg_listen_port))
}

pub fn derive_prefix_from_secret(secret: &MeshSecret) -> std::net::Ipv6Addr {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest([b"mesh-prefix:" as &[u8], secret.as_bytes()].concat());
    std::net::Ipv6Addr::new(
        0xfd00 | (hash[0] as u16),
        ((hash[1] as u16) << 8) | (hash[2] as u16),
        ((hash[3] as u16) << 8) | (hash[4] as u16),
        0,
        0,
        0,
        0,
        0,
    )
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Compute the health-check timeout for a specific peer based on topology
/// proximity to the local node.
///
/// - Same zone → `same_zone_timeout`
/// - Same region, different zone → `same_region_timeout`
/// - Different region or unknown topology → `cross_region_timeout`
pub fn timeout_for_peer(
    my_topo: &Option<syfrah_core::mesh::Topology>,
    peer_topo: &Option<syfrah_core::mesh::Topology>,
    policy: &config::HealthPolicy,
) -> u64 {
    match (my_topo, peer_topo) {
        (Some(mine), Some(theirs)) => {
            if mine.region == theirs.region && mine.zone == theirs.zone {
                policy.same_zone_timeout.as_secs()
            } else if mine.region == theirs.region {
                policy.same_region_timeout.as_secs()
            } else {
                policy.cross_region_timeout.as_secs()
            }
        }
        // Unknown topology → safest (longest) timeout
        _ => policy.cross_region_timeout.as_secs(),
    }
}

/// Evaluate a single peer's health state based on handshake data and timeout.
///
/// Updates `peer.last_seen` and `peer.status` as appropriate.
/// Returns `true` if any field was changed.
pub fn evaluate_peer_health(
    peer: &mut PeerRecord,
    wg_handshake_epoch: Option<u64>,
    current_time: u64,
    unreachable_timeout_secs: u64,
) -> bool {
    let mut changed = false;

    // Update last_seen from WireGuard handshake timestamp
    if let Some(handshake_epoch) = wg_handshake_epoch {
        if handshake_epoch > peer.last_seen {
            peer.last_seen = handshake_epoch;
            changed = true;
        }
    }

    // Recovery: unreachable → active if recent handshake
    if peer.status == PeerStatus::Unreachable
        && current_time.saturating_sub(peer.last_seen) < unreachable_timeout_secs
    {
        peer.status = PeerStatus::Active;
        changed = true;
    }

    // Detection: active → unreachable if no handshake for too long
    if peer.status == PeerStatus::Active
        && current_time.saturating_sub(peer.last_seen) > unreachable_timeout_secs
    {
        peer.status = PeerStatus::Unreachable;
        changed = true;
    }

    changed
}

/// Determine which stored peers are missing from WireGuard and need reconciliation.
///
/// Returns references to peers that should be re-added (non-Removed peers not in WG).
pub fn peers_needing_reconciliation<'a>(
    stored: &'a [PeerRecord],
    wg_keys: &[String],
) -> Vec<&'a PeerRecord> {
    stored
        .iter()
        .filter(|peer| peer.status != PeerStatus::Removed && !wg_keys.contains(&peer.wg_public_key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv6Addr, SocketAddr};
    use syfrah_core::mesh::Topology;
    use wireguard_control::KeyPair;

    fn sample_peer(name: &str, status: PeerStatus, last_seen: u64) -> PeerRecord {
        PeerRecord {
            name: name.to_string(),
            wg_public_key: format!("key-{name}"),
            endpoint: "203.0.113.1:51820".parse::<SocketAddr>().unwrap(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 1),
            last_seen,
            status,
            region: None,
            zone: None,
            topology: None,
        }
    }

    // ── derive_prefix_from_secret tests ──

    #[test]
    fn derive_prefix_deterministic() {
        let secret = MeshSecret::from_bytes([42u8; 32]);
        let p1 = derive_prefix_from_secret(&secret);
        let p2 = derive_prefix_from_secret(&secret);
        assert_eq!(p1, p2);
    }

    #[test]
    fn derive_prefix_unique_for_different_secrets() {
        let s1 = MeshSecret::from_bytes([1u8; 32]);
        let s2 = MeshSecret::from_bytes([2u8; 32]);
        let p1 = derive_prefix_from_secret(&s1);
        let p2 = derive_prefix_from_secret(&s2);
        assert_ne!(p1, p2);
    }

    #[test]
    fn derive_prefix_known_value() {
        // The prefix must be in the fd00::/8 ULA range
        let secret = MeshSecret::from_bytes([0u8; 32]);
        let prefix = derive_prefix_from_secret(&secret);
        let segments = prefix.segments();
        // First nibble of first segment must be 0xfd
        assert_eq!(
            segments[0] >> 8,
            0xfd,
            "prefix must be in fd00::/8 ULA range"
        );
        // Last 4 segments must be zero (it's a /48 prefix)
        assert_eq!(segments[4], 0);
        assert_eq!(segments[5], 0);
        assert_eq!(segments[6], 0);
        assert_eq!(segments[7], 0);
    }

    // ── build_record tests ──

    #[test]
    fn build_record_fields() {
        let keypair = KeyPair::generate();
        let endpoint: SocketAddr = "10.0.0.1:51820".parse().unwrap();
        let ipv6 = Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1);

        let record = build_record(
            "node-1",
            &keypair,
            endpoint,
            ipv6,
            Some("us-east"),
            Some("us-east-1a"),
        );

        assert_eq!(record.name, "node-1");
        assert_eq!(record.wg_public_key, keypair.public.to_base64());
        assert_eq!(record.endpoint, endpoint);
        assert_eq!(record.mesh_ipv6, ipv6);
        assert_eq!(record.status, PeerStatus::Active);
        assert_eq!(record.region.as_deref(), Some("us-east"));
        assert_eq!(record.zone.as_deref(), Some("us-east-1a"));
    }

    #[test]
    fn build_record_timestamp_is_recent() {
        let keypair = KeyPair::generate();
        let endpoint: SocketAddr = "10.0.0.1:51820".parse().unwrap();
        let ipv6 = Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1);

        let before = now();
        let record = build_record("node-1", &keypair, endpoint, ipv6, None, None);
        let after = now();

        assert!(record.last_seen >= before);
        assert!(record.last_seen <= after);
    }

    // ── resolve_endpoint tests ──

    #[test]
    fn resolve_endpoint_default_fallback() {
        let config = DaemonConfig {
            mesh_name: "test".into(),
            node_name: "node".into(),
            wg_listen_port: 51820,
            public_endpoint: None,
            peering_port: 7946,
            region: None,
            zone: None,
        };
        let ep = resolve_endpoint(&config);
        assert_eq!(ep, "0.0.0.0:51820".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn resolve_endpoint_custom() {
        let custom: SocketAddr = "203.0.113.5:9999".parse().unwrap();
        let config = DaemonConfig {
            mesh_name: "test".into(),
            node_name: "node".into(),
            wg_listen_port: 51820,
            public_endpoint: Some(custom),
            peering_port: 7946,
            region: None,
            zone: None,
        };
        let ep = resolve_endpoint(&config);
        assert_eq!(ep, custom);
    }

    // ── now() test ──

    #[test]
    fn now_returns_reasonable_value() {
        let t = now();
        // Should be after 2024-01-01 and before 2100-01-01
        assert!(t > 1_704_067_200, "now() should be after 2024-01-01");
        assert!(t < 4_102_444_800, "now() should be before 2100-01-01");
    }

    // ── evaluate_peer_health tests ──

    #[test]
    fn health_active_to_unreachable_after_timeout() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        // current_time = 1301 (301s after last_seen), timeout = 300
        let changed = evaluate_peer_health(&mut peer, None, 1301, 300);
        assert!(changed);
        assert_eq!(peer.status, PeerStatus::Unreachable);
    }

    #[test]
    fn health_active_stays_active_before_timeout() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        // current_time = 1299 (299s after last_seen), timeout = 300
        let changed = evaluate_peer_health(&mut peer, None, 1299, 300);
        assert!(!changed);
        assert_eq!(peer.status, PeerStatus::Active);
    }

    #[test]
    fn health_active_stays_active_at_exact_boundary() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        // current_time = 1300 (exactly 300s after last_seen), timeout = 300
        // saturating_sub(1300, 1000) = 300, which is NOT > 300, so stays active
        let changed = evaluate_peer_health(&mut peer, None, 1300, 300);
        assert!(!changed);
        assert_eq!(peer.status, PeerStatus::Active);
    }

    #[test]
    fn health_unreachable_to_active_on_recent_handshake() {
        let mut peer = sample_peer("node-2", PeerStatus::Unreachable, 1000);
        // Handshake at 1260 (current - 1260 = 40s ago), timeout = 300
        let changed = evaluate_peer_health(&mut peer, Some(1260), 1300, 300);
        assert!(changed);
        assert_eq!(peer.status, PeerStatus::Active);
        assert_eq!(peer.last_seen, 1260);
    }

    #[test]
    fn health_unreachable_stays_unreachable_no_recent_handshake() {
        let mut peer = sample_peer("node-2", PeerStatus::Unreachable, 1000);
        // current_time = 1400, no new handshake, timeout = 300
        // 1400 - 1000 = 400 >= 300, stays unreachable
        let changed = evaluate_peer_health(&mut peer, None, 1400, 300);
        assert!(!changed);
        assert_eq!(peer.status, PeerStatus::Unreachable);
    }

    // ── last_seen update tests ──

    #[test]
    fn health_updates_last_seen_from_newer_handshake() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        let changed = evaluate_peer_health(&mut peer, Some(1050), 1100, 300);
        assert!(changed);
        assert_eq!(peer.last_seen, 1050);
    }

    #[test]
    fn health_does_not_update_last_seen_from_older_handshake() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        let changed = evaluate_peer_health(&mut peer, Some(900), 1100, 300);
        assert!(!changed);
        assert_eq!(peer.last_seen, 1000);
    }

    #[test]
    fn health_no_handshake_no_update() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        let changed = evaluate_peer_health(&mut peer, None, 1100, 300);
        assert!(!changed);
        assert_eq!(peer.last_seen, 1000);
    }

    // ── peers_needing_reconciliation tests ──

    #[test]
    fn reconciliation_missing_peer_needs_readd() {
        let peers = vec![
            sample_peer("node-1", PeerStatus::Active, 1000),
            sample_peer("node-2", PeerStatus::Active, 1000),
        ];
        let wg_keys = vec!["key-node-1".to_string()]; // node-2 is missing
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        assert_eq!(needing.len(), 1);
        assert_eq!(needing[0].name, "node-2");
    }

    #[test]
    fn reconciliation_present_peer_no_action() {
        let peers = vec![sample_peer("node-1", PeerStatus::Active, 1000)];
        let wg_keys = vec!["key-node-1".to_string()];
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        assert!(needing.is_empty());
    }

    #[test]
    fn reconciliation_removed_peer_not_readded() {
        let peers = vec![sample_peer("node-1", PeerStatus::Removed, 1000)];
        let wg_keys = vec![]; // not in WG, but it's Removed
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        assert!(needing.is_empty());
    }

    // ── map_join_error tests ──

    #[test]
    fn map_join_error_unexpected_eof_is_friendly() {
        let io_err = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "early eof");
        let peering_err = crate::peering::PeeringError::Io(io_err);
        let target: SocketAddr = "203.0.113.1:51821".parse().unwrap();
        let mapped = map_join_error(peering_err, target);
        let msg = mapped.to_string();
        assert!(
            msg.contains("Connection closed by"),
            "expected friendly message, got: {msg}"
        );
        assert!(
            msg.contains("peering"),
            "should suggest peering, got: {msg}"
        );
    }

    #[test]
    fn map_join_error_timeout_is_friendly() {
        let peering_err = crate::peering::PeeringError::Timeout;
        let target: SocketAddr = "203.0.113.1:51821".parse().unwrap();
        let mapped = map_join_error(peering_err, target);
        let msg = mapped.to_string();
        assert!(
            msg.contains("timed out"),
            "expected timeout message, got: {msg}"
        );
        assert!(
            msg.contains("peering"),
            "should suggest peering, got: {msg}"
        );
    }

    #[test]
    fn map_join_error_connection_refused_is_friendly() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let peering_err = crate::peering::PeeringError::Io(io_err);
        let target: SocketAddr = "203.0.113.1:51821".parse().unwrap();
        let mapped = map_join_error(peering_err, target);
        let msg = mapped.to_string();
        assert!(
            msg.contains("Could not connect to"),
            "expected connection context, got: {msg}"
        );
        assert!(
            msg.contains("peering enabled"),
            "should suggest peering, got: {msg}"
        );
    }

    #[test]
    fn map_join_error_connection_reset_is_friendly() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let peering_err = crate::peering::PeeringError::Io(io_err);
        let target: SocketAddr = "203.0.113.1:51821".parse().unwrap();
        let mapped = map_join_error(peering_err, target);
        let msg = mapped.to_string();
        assert!(
            msg.contains("Could not connect to"),
            "expected connection context, got: {msg}"
        );
        assert!(
            msg.contains("peering enabled"),
            "should suggest peering, got: {msg}"
        );
    }

    #[test]
    fn map_join_error_generic_io_includes_context() {
        let io_err = std::io::Error::other("network down");
        let peering_err = crate::peering::PeeringError::Io(io_err);
        let target: SocketAddr = "203.0.113.1:51821".parse().unwrap();
        let mapped = map_join_error(peering_err, target);
        let msg = mapped.to_string();
        assert!(
            msg.contains("Could not connect to"),
            "expected connection context, got: {msg}"
        );
        assert!(
            msg.contains("network down"),
            "should include original error, got: {msg}"
        );
    }

    #[test]
    fn map_join_error_other_passes_through() {
        let peering_err = crate::peering::PeeringError::Protocol("something weird".into());
        let target: SocketAddr = "203.0.113.1:51821".parse().unwrap();
        let mapped = map_join_error(peering_err, target);
        let msg = mapped.to_string();
        assert!(
            msg.contains("something weird"),
            "other errors pass through, got: {msg}"
        );
    }

    #[test]
    fn rollback_join_state_is_safe_when_no_state() {
        // rollback should not panic even when there's nothing to clean up
        rollback_join_state();
    }

    // ── resolve_region_zone tests (exercises the real production function) ──

    #[test]
    fn resolve_region_zone_defaults_when_none() {
        let (region, zone) = super::resolve_region_zone(None, None, &[]);
        assert_eq!(
            region, "default",
            "region should fall back to DEFAULT_REGION"
        );
        assert_eq!(
            zone, "zone-1",
            "zone should be auto-generated as zone-1 with no peers"
        );
    }

    #[test]
    fn resolve_region_zone_explicit_region_overrides_default() {
        let (region, zone) = super::resolve_region_zone(Some("us-east"), None, &[]);
        assert_eq!(region, "us-east");
        assert_eq!(zone, "zone-1", "zone should still be auto-generated");
    }

    #[test]
    fn resolve_region_zone_explicit_zone_overrides_generation() {
        let (region, zone) = super::resolve_region_zone(Some("eu-west"), Some("eu-west-a"), &[]);
        assert_eq!(region, "eu-west");
        assert_eq!(zone, "eu-west-a", "explicit zone should be used as-is");
    }

    #[test]
    fn resolve_region_zone_increments_with_existing_peers() {
        use syfrah_core::mesh::{PeerRecord, PeerStatus};
        let peers = vec![PeerRecord {
            name: "node-1".into(),
            wg_public_key: "key".into(),
            endpoint: "127.0.0.1:51820".parse().unwrap(),
            mesh_ipv6: "fd00::1".parse().unwrap(),
            last_seen: 0,
            status: PeerStatus::Active,
            region: Some("default".into()),
            zone: Some("zone-1".into()),
            topology: None,
        }];
        let (region, zone) = super::resolve_region_zone(None, None, &peers);
        assert_eq!(region, "default");
        assert_eq!(
            zone, "zone-2",
            "zone index should increment past existing peers"
        );
    }

    // ── reconciliation edge cases ──

    #[test]
    fn reconciliation_empty_stored_peers() {
        let peers: Vec<PeerRecord> = vec![];
        let wg_keys = vec!["key-node-1".to_string()];
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        assert!(needing.is_empty());
    }

    #[test]
    fn reconciliation_empty_wg_keys_returns_all_active() {
        let peers = vec![
            sample_peer("node-1", PeerStatus::Active, 1000),
            sample_peer("node-2", PeerStatus::Active, 1000),
        ];
        let wg_keys: Vec<String> = vec![];
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        assert_eq!(needing.len(), 2);
    }

    #[test]
    fn reconciliation_unreachable_peer_still_reconciled() {
        // Unreachable peers should be re-added to WG (only Removed are skipped)
        let peers = vec![sample_peer("node-1", PeerStatus::Unreachable, 1000)];
        let wg_keys: Vec<String> = vec![];
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        assert_eq!(needing.len(), 1);
        assert_eq!(needing[0].name, "node-1");
    }

    #[test]
    fn reconciliation_mixed_statuses() {
        let peers = vec![
            sample_peer("active-1", PeerStatus::Active, 1000),
            sample_peer("unreach-1", PeerStatus::Unreachable, 1000),
            sample_peer("removed-1", PeerStatus::Removed, 1000),
        ];
        // None are in WG
        let wg_keys: Vec<String> = vec![];
        let needing = peers_needing_reconciliation(&peers, &wg_keys);
        // Only active and unreachable should be reconciled, not removed
        assert_eq!(needing.len(), 2);
        let names: Vec<&str> = needing.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"active-1"));
        assert!(names.contains(&"unreach-1"));
        assert!(!names.contains(&"removed-1"));
    }

    // ── health check: never remove peers with recent handshake ──

    #[test]
    fn health_check_never_marks_removed_if_recent_handshake() {
        // A peer with a recent handshake should never transition to Removed,
        // even if it was previously Unreachable.
        let mut peer = sample_peer("node-1", PeerStatus::Unreachable, 1000);
        // Fresh handshake at 1250, current time 1260, timeout 300
        let changed = evaluate_peer_health(&mut peer, Some(1250), 1260, 300);
        assert!(changed);
        assert_eq!(
            peer.status,
            PeerStatus::Active,
            "peer with recent handshake must recover to Active, not be removed"
        );
        assert_eq!(peer.last_seen, 1250);
    }

    #[test]
    fn health_check_active_peer_with_handshake_stays_active() {
        // An active peer that has a recent handshake should stay active
        let mut peer = sample_peer("node-1", PeerStatus::Active, 900);
        // Handshake at 1100, current time 1200, timeout 300
        // Without handshake: 1200-900=300 which is NOT > 300 (stays active)
        // With handshake: last_seen updated to 1100, 1200-1100=100 < 300
        let changed = evaluate_peer_health(&mut peer, Some(1100), 1200, 300);
        assert!(changed); // last_seen updated
        assert_eq!(peer.status, PeerStatus::Active);
        assert_eq!(peer.last_seen, 1100);
    }

    // ── health check lifecycle ──

    #[test]
    fn health_full_lifecycle_active_unreachable_active() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);

        // Step 1: active, still within timeout — no change
        let changed = evaluate_peer_health(&mut peer, None, 1100, 300);
        assert!(!changed);
        assert_eq!(peer.status, PeerStatus::Active);

        // Step 2: active → unreachable (timeout exceeded)
        let changed = evaluate_peer_health(&mut peer, None, 1301, 300);
        assert!(changed);
        assert_eq!(peer.status, PeerStatus::Unreachable);

        // Step 3: unreachable → active (fresh handshake arrives)
        let changed = evaluate_peer_health(&mut peer, Some(1350), 1360, 300);
        assert!(changed);
        assert_eq!(peer.status, PeerStatus::Active);
        assert_eq!(peer.last_seen, 1350);
    }

    #[test]
    fn health_multiple_peers_independent() {
        let mut peer_a = sample_peer("node-a", PeerStatus::Active, 1000);
        let mut peer_b = sample_peer("node-b", PeerStatus::Active, 1200);

        // At time 1301: peer_a times out (1301-1000=301>300), peer_b is fine (1301-1200=101<300)
        let a_changed = evaluate_peer_health(&mut peer_a, None, 1301, 300);
        let b_changed = evaluate_peer_health(&mut peer_b, None, 1301, 300);

        assert!(a_changed);
        assert_eq!(peer_a.status, PeerStatus::Unreachable);
        assert!(!b_changed);
        assert_eq!(peer_b.status, PeerStatus::Active);
    }

    #[test]
    fn health_handshake_at_exact_last_seen_no_update() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        // Handshake at exactly last_seen — not newer, so no update
        let changed = evaluate_peer_health(&mut peer, Some(1000), 1100, 300);
        assert!(!changed);
        assert_eq!(peer.last_seen, 1000);
    }

    #[test]
    fn health_zero_timestamps() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 0);
        // With last_seen=0, current=0, timeout=300: 0-0=0 which is not > 300
        let changed = evaluate_peer_health(&mut peer, None, 0, 300);
        assert!(!changed);
        assert_eq!(peer.status, PeerStatus::Active);
    }

    #[test]
    fn health_handshake_updates_then_prevents_unreachable() {
        let mut peer = sample_peer("node-2", PeerStatus::Active, 1000);
        // Without handshake at time 1301, peer would go unreachable.
        // But a handshake at 1100 updates last_seen, keeping gap at 201 < 300.
        let changed = evaluate_peer_health(&mut peer, Some(1100), 1301, 300);
        assert!(changed); // last_seen updated
        assert_eq!(peer.last_seen, 1100);
        assert_eq!(peer.status, PeerStatus::Active);
    }

    // ── build_record edge cases ──

    #[test]
    fn build_record_no_region_no_zone() {
        let keypair = KeyPair::generate();
        let endpoint: SocketAddr = "10.0.0.1:51820".parse().unwrap();
        let ipv6 = Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1);

        let record = build_record("node-1", &keypair, endpoint, ipv6, None, None);

        assert!(record.region.is_none());
        assert!(record.zone.is_none());
    }

    // ── derive_prefix edge cases ──

    #[test]
    fn derive_prefix_always_ula_range() {
        // Every secret should produce an fd00::/8 prefix
        for i in 0..10u8 {
            let secret = MeshSecret::from_bytes([i; 32]);
            let prefix = derive_prefix_from_secret(&secret);
            let first_byte = (prefix.segments()[0] >> 8) as u8;
            assert_eq!(first_byte, 0xfd, "secret [{i};32] gave non-ULA prefix");
        }
    }

    #[test]
    fn derive_prefix_host_segments_always_zero() {
        // The prefix is /48, so segments 3..7 must always be zero
        for i in 0..5u8 {
            let secret = MeshSecret::from_bytes([i * 50; 32]);
            let prefix = derive_prefix_from_secret(&secret);
            let segs = prefix.segments();
            assert_eq!(segs[3], 0, "segment 3 non-zero for secret [{};32]", i * 50);
            assert_eq!(segs[4], 0, "segment 4 non-zero for secret [{};32]", i * 50);
            assert_eq!(segs[5], 0, "segment 5 non-zero for secret [{};32]", i * 50);
            assert_eq!(segs[6], 0, "segment 6 non-zero for secret [{};32]", i * 50);
            assert_eq!(segs[7], 0, "segment 7 non-zero for secret [{};32]", i * 50);
        }
    }

    // ── Phantom peer / endpoint validation tests (issue #285) ──

    #[test]
    fn resolve_endpoint_returns_unspecified_when_no_public_endpoint() {
        let config = DaemonConfig {
            mesh_name: "test".into(),
            node_name: "node".into(),
            wg_listen_port: 51820,
            public_endpoint: None,
            peering_port: 7946,
            region: None,
            zone: None,
        };
        let ep = resolve_endpoint(&config);
        assert!(
            ep.ip().is_unspecified(),
            "with no --endpoint, resolve_endpoint should return 0.0.0.0"
        );
    }

    #[test]
    fn unspecified_endpoint_is_detectable() {
        // Verify that our is_unspecified() check works as expected
        // for both IPv4 and IPv6 unspecified addresses.
        let v4_zero: SocketAddr = "0.0.0.0:51820".parse().unwrap();
        assert!(v4_zero.ip().is_unspecified());

        let v6_zero: SocketAddr = "[::]:51820".parse().unwrap();
        assert!(v6_zero.ip().is_unspecified());

        let real: SocketAddr = "65.21.178.96:51820".parse().unwrap();
        assert!(!real.ip().is_unspecified());
    }

    #[test]
    fn self_endpoint_detection() {
        // Verify that we can detect when a peer's endpoint matches
        // the local node's own public IP.
        let my_endpoint: SocketAddr = "65.21.178.96:51820".parse().unwrap();
        let peer_endpoint: SocketAddr = "65.21.178.96:51820".parse().unwrap();
        let other_endpoint: SocketAddr = "65.21.140.60:51820".parse().unwrap();

        assert_eq!(
            peer_endpoint.ip(),
            my_endpoint.ip(),
            "peer with same IP as self should be detected"
        );
        assert_ne!(
            other_endpoint.ip(),
            my_endpoint.ip(),
            "peer with different IP should not be flagged"
        );
    }

    // ── timeout_for_peer tests ──

    fn test_health_policy() -> config::HealthPolicy {
        config::HealthPolicy {
            same_zone_timeout: std::time::Duration::from_secs(120),
            same_region_timeout: std::time::Duration::from_secs(180),
            cross_region_timeout: std::time::Duration::from_secs(300),
        }
    }

    #[test]
    fn timeout_same_zone() {
        let topo_a = Topology::from_strings(Some("eu-west"), Some("zone-a"));
        let topo_b = Topology::from_strings(Some("eu-west"), Some("zone-a"));
        let policy = test_health_policy();
        assert_eq!(timeout_for_peer(&topo_a, &topo_b, &policy), 120);
    }

    #[test]
    fn timeout_same_region_different_zone() {
        let topo_a = Topology::from_strings(Some("eu-west"), Some("zone-a"));
        let topo_b = Topology::from_strings(Some("eu-west"), Some("zone-b"));
        let policy = test_health_policy();
        assert_eq!(timeout_for_peer(&topo_a, &topo_b, &policy), 180);
    }

    #[test]
    fn timeout_cross_region() {
        let topo_a = Topology::from_strings(Some("eu-west"), Some("zone-a"));
        let topo_b = Topology::from_strings(Some("us-east"), Some("zone-a"));
        let policy = test_health_policy();
        assert_eq!(timeout_for_peer(&topo_a, &topo_b, &policy), 300);
    }

    #[test]
    fn timeout_peer_no_topology_uses_cross_region() {
        let topo_a = Topology::from_strings(Some("eu-west"), Some("zone-a"));
        let policy = test_health_policy();
        assert_eq!(timeout_for_peer(&topo_a, &None, &policy), 300);
    }

    #[test]
    fn timeout_self_no_topology_uses_cross_region() {
        let topo_b = Topology::from_strings(Some("eu-west"), Some("zone-a"));
        let policy = test_health_policy();
        assert_eq!(timeout_for_peer(&None, &topo_b, &policy), 300);
    }

    #[test]
    fn timeout_both_no_topology_uses_cross_region() {
        let policy = test_health_policy();
        assert_eq!(timeout_for_peer(&None, &None, &policy), 300);
    }
}
