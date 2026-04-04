use crate::control::{send_control_request, FabricRequest, FabricResponse};
use crate::daemon::{self, DaemonConfig};
use crate::peering::generate_pin;
use crate::store;
use anyhow::{Context, Result};
use std::net::SocketAddr;

pub async fn run(
    name: &str,
    node_name: &str,
    port: u16,
    endpoint: Option<SocketAddr>,
    peering_port: u16,
    region: Option<String>,
    zone: Option<String>,
) -> Result<()> {
    daemon::run_init(DaemonConfig {
        mesh_name: name.to_string(),
        node_name: node_name.to_string(),
        wg_listen_port: port,
        public_endpoint: endpoint,
        peering_port,
        region,
        zone,
        no_hypervisor: false,
    })
    .await
    .context("Failed to initialize mesh. If a mesh already exists, run: syfrah fabric leave")
}

/// Wait for the daemon control socket, then start peering with a generated PIN.
/// Called from the parent process after daemonize().
pub async fn wait_and_start_peering(endpoint: Option<SocketAddr>, peering_port: u16) -> Result<()> {
    let socket_path = store::control_socket_path();

    // Wait for control socket to appear (daemon starting up)
    let mut ready = false;
    for _ in 0..50 {
        if socket_path.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !ready {
        anyhow::bail!("timed out waiting for daemon to start");
    }

    // Small extra delay for the socket to be fully listening
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let pin = generate_pin();

    let resp = send_control_request(
        &socket_path,
        &FabricRequest::PeeringStart {
            port: peering_port,
            pin: Some(pin.clone()),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to start peering via control socket: {e}"))?;

    match resp {
        FabricResponse::Ok => {}
        FabricResponse::Error { message } => anyhow::bail!("peering start failed: {message}"),
        _ => anyhow::bail!("unexpected response from daemon"),
    }

    // Load state to get mesh name and secret for display
    let state = store::load()?;

    println!("Mesh '{}' created.", state.mesh_name);
    println!("  Secret: {}", state.mesh_secret);
    println!("  PIN:    {pin}");
    println!();
    println!("Daemon started. Peering active.");
    println!();

    // Print the join command
    let ip_str = if let Some(ep) = endpoint {
        ep.ip().to_string()
    } else {
        "<IP>".to_string()
    };

    println!("Share this with other servers:");
    println!("  syfrah fabric join {ip_str} --pin {pin}");

    Ok(())
}
