//! Control plane operations — high-level orchestration.
//!
//! Called by the hypervisor handler during init/join/leave/status.
//! Orchestrates TiUP install → config → systemd → health check.
//!
//! PD (Raft consensus) runs on max 3 nodes for optimal performance.
//! Additional nodes run TiKV only (storage), connecting to existing PD.

use std::net::Ipv6Addr;

use syfrah_core::error::SyfrahError;

use super::service::{self, PdConfig, TikvConfig};

/// Max PD members in the cluster. Raft works best with 3 or 5.
const MAX_PD_MEMBERS: usize = 3;

/// Bootstrap a new single-node TiKV cluster.
pub fn bootstrap(
    node_name: &str,
    mesh_ipv6: &Ipv6Addr,
) -> Result<(), SyfrahError> {
    eprintln!("  Setting up control plane...");

    service::ensure_installed()?;

    let pd_cfg = PdConfig {
        name: node_name.to_string(),
        mesh_ipv6: *mesh_ipv6,
        initial_cluster: format!(
            "{node_name}=http://[{mesh_ipv6}]:{}",
            super::PD_PEER_PORT
        ),
        initial_cluster_state: "new".to_string(),
    };

    let tikv_cfg = TikvConfig {
        mesh_ipv6: *mesh_ipv6,
        pd_endpoints: vec![format!(
            "http://[{mesh_ipv6}]:{}",
            super::PD_CLIENT_PORT
        )],
    };

    service::install(&pd_cfg, &tikv_cfg, None)?;
    service::enable_and_start()?;

    eprintln!("  Waiting for PD...");
    service::wait_pd_ready(mesh_ipv6, 30)?;
    eprintln!("  Waiting for TiKV...");
    service::wait_tikv_ready(mesh_ipv6, 60)?;

    eprintln!("  Control plane ready");
    Ok(())
}

/// Join an existing TiKV cluster.
///
/// If there are fewer than MAX_PD_MEMBERS, this node runs PD + TiKV.
/// Otherwise, this node runs TiKV only (connecting to existing PD).
/// Join an existing TiKV cluster.
///
/// `peer_count` is the number of peers already known (from fabric join).
/// - 0-1 peers → this is the 2nd or 3rd node → run PD + TiKV
/// - 2+ peers → 3+ nodes already exist → run TiKV only
///
/// This avoids the race condition of querying PD API during rapid joins.
pub fn join(
    node_name: &str,
    mesh_ipv6: &Ipv6Addr,
    existing_pd_endpoints: &[String],
    peer_count: usize,
) -> Result<(), SyfrahError> {
    if existing_pd_endpoints.is_empty() {
        return Err(SyfrahError::precondition(
            "no PD endpoints available. Cannot join control plane.",
        ));
    }

    eprintln!("  Joining control plane...");
    service::ensure_installed()?;

    let primary_pd = &existing_pd_endpoints[0];

    // Use peer count to decide: first 2 joiners run PD, rest run TiKV-only
    // peer_count=1 means we're the 2nd node (1 peer = the init node)
    // peer_count=2 means we're the 3rd node (2 peers)
    // peer_count>=3 means 4th+ node → TiKV only
    let run_pd = peer_count < (MAX_PD_MEMBERS - 1); // -1 because init node already has PD

    if run_pd {
        eprintln!("  Starting PD + TiKV (node {} of {})", peer_count + 1, MAX_PD_MEMBERS);
        join_with_pd(node_name, mesh_ipv6, existing_pd_endpoints, primary_pd)?;
    } else {
        eprintln!("  Starting TiKV only (PD cluster full at {MAX_PD_MEMBERS} members)");
        join_tikv_only(node_name, mesh_ipv6, existing_pd_endpoints)?;
    }

    eprintln!("  Control plane joined");
    Ok(())
}

/// Join with both PD and TiKV (for first 3 nodes).
fn join_with_pd(
    node_name: &str,
    mesh_ipv6: &Ipv6Addr,
    existing_pd_endpoints: &[String],
    primary_pd: &str,
) -> Result<(), SyfrahError> {
    // Wait for mesh connectivity to PD before attempting join
    eprintln!("  Verifying mesh connectivity to PD...");
    wait_mesh_connectivity(primary_pd, 30)?;

    let self_peer_url = format!("http://[{mesh_ipv6}]:{}", super::PD_PEER_PORT);

    let pd_cfg = PdConfig {
        name: node_name.to_string(),
        mesh_ipv6: *mesh_ipv6,
        initial_cluster: format!("{node_name}={self_peer_url}"),
        initial_cluster_state: "join".to_string(),
    };

    let mut pd_endpoints: Vec<String> = existing_pd_endpoints.to_vec();
    let self_endpoint = format!("http://[{mesh_ipv6}]:{}", super::PD_CLIENT_PORT);
    if !pd_endpoints.contains(&self_endpoint) {
        pd_endpoints.push(self_endpoint);
    }

    let tikv_cfg = TikvConfig {
        mesh_ipv6: *mesh_ipv6,
        pd_endpoints,
    };

    service::install(&pd_cfg, &tikv_cfg, Some(primary_pd))?;
    service::enable_and_start()?;

    eprintln!("  Waiting for PD...");
    service::wait_pd_ready(mesh_ipv6, 120)?;
    eprintln!("  Waiting for TiKV...");
    service::wait_tikv_ready(mesh_ipv6, 120)?;

    Ok(())
}

/// Join with TiKV only (for nodes beyond MAX_PD_MEMBERS).
fn join_tikv_only(
    _node_name: &str,
    mesh_ipv6: &Ipv6Addr,
    existing_pd_endpoints: &[String],
) -> Result<(), SyfrahError> {
    // Wait for mesh connectivity to PD
    eprintln!("  Verifying mesh connectivity to PD...");
    wait_mesh_connectivity(&existing_pd_endpoints[0], 30)?;

    let tikv_cfg = TikvConfig {
        mesh_ipv6: *mesh_ipv6,
        pd_endpoints: existing_pd_endpoints.to_vec(),
    };

    // Install TiKV only (no PD unit)
    service::install_tikv_only(&tikv_cfg)?;

    // Start TiKV only
    service::start_tikv_only()?;

    // Wait for TiKV to register with existing PD
    // Use first PD endpoint's IP for health check
    let pd_ip = extract_ipv6_from_endpoint(&existing_pd_endpoints[0]);
    if let Some(ip) = pd_ip {
        eprintln!("  Waiting for TiKV...");
        service::wait_tikv_ready(&ip, 60)?;
    }

    Ok(())
}

/// Get current cluster status.
pub fn status(mesh_ipv6: &Ipv6Addr) -> Result<service::ClusterStatus, SyfrahError> {
    service::cluster_status(mesh_ipv6)
}

/// Start the control plane services.
pub fn start() -> Result<(), SyfrahError> {
    if !service::is_installed() {
        return Err(SyfrahError::precondition(
            "control plane not installed. Run 'syfrah hypervisor init' first.",
        ));
    }
    service::start()
}

/// Stop the control plane services.
pub fn stop() -> Result<(), SyfrahError> {
    service::stop()
}

/// Restart the control plane services.
pub fn restart() -> Result<(), SyfrahError> {
    service::restart()
}

/// Uninstall the control plane. Deregisters TiKV store first.
pub fn leave_with_mesh(mesh_ipv6: &Ipv6Addr) -> Result<(), SyfrahError> {
    // Deregister TiKV store before stopping
    let _ = service::deregister_store(mesh_ipv6);
    service::uninstall()
}

/// Uninstall without deregistration (fallback).
pub fn leave() -> Result<(), SyfrahError> {
    service::uninstall()
}

/// Wait for mesh connectivity to a PD endpoint (HTTP health check).
fn wait_mesh_connectivity(pd_url: &str, timeout_secs: u64) -> Result<(), SyfrahError> {
    let url = format!("{pd_url}/pd/api/v1/health");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    while std::time::Instant::now() < deadline {
        if std::process::Command::new("curl")
            .args(["-sf", "--max-time", "3", &url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            // Also clean up any zombie PD members before joining
            cleanup_zombie_members(pd_url);
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    Err(SyfrahError::timeout("mesh connectivity to PD", timeout_secs))
}

/// Remove unhealthy PD members (zombies from failed joins).
fn cleanup_zombie_members(pd_url: &str) {
    let url = format!("{pd_url}/pd/api/v1/health");
    let output = match std::process::Command::new("curl")
        .args(["-sf", "--max-time", "5", &url])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };

    let health: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return,
    };

    if let Some(members) = health.as_array() {
        for member in members {
            let healthy = member["health"].as_bool().unwrap_or(true);
            let name = member["name"].as_str().unwrap_or("");
            let member_id = member["member_id"].as_u64().unwrap_or(0);

            // Remove unhealthy members with no name (zombie from failed join)
            if !healthy && name.is_empty() && member_id > 0 {
                eprintln!("  Removing zombie PD member {member_id}...");
                let delete_url = format!("{pd_url}/pd/api/v1/members/id/{member_id}");
                let _ = std::process::Command::new("curl")
                    .args(["-sf", "-X", "DELETE", "--max-time", "5", &delete_url])
                    .output();
            }
        }
    }
}

/// Extract IPv6 from endpoint like "http://[fd01::1]:2379"
fn extract_ipv6_from_endpoint(endpoint: &str) -> Option<Ipv6Addr> {
    let s = endpoint.strip_prefix("http://[")?;
    let s = s.split(']').next()?;
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::service::{PdConfig, TikvConfig};

    #[test]
    fn bootstrap_config_single_node() {
        let ip: Ipv6Addr = "fd01::1".parse().unwrap();
        let pd_cfg = PdConfig {
            name: "node-1".into(),
            mesh_ipv6: ip,
            initial_cluster: format!("node-1=http://[{ip}]:2380"),
            initial_cluster_state: "new".into(),
        };
        let conf = service::generate_pd_conf(&pd_cfg, false);
        assert!(conf.contains("initial-cluster-state = \"new\""));
    }

    #[test]
    fn join_config_format() {
        let ip: Ipv6Addr = "fd01::2".parse().unwrap();
        let pd_cfg = PdConfig {
            name: "node-2".into(),
            mesh_ipv6: ip,
            initial_cluster: "node-2=http://[fd01::2]:2380".into(),
            initial_cluster_state: "join".into(),
        };
        let conf = service::generate_pd_conf(&pd_cfg, true);
        assert!(!conf.contains("initial-cluster"));
        assert!(conf.contains("node-2"));
    }

    #[test]
    fn tikv_multi_pd_endpoints() {
        let cfg = TikvConfig {
            mesh_ipv6: "fd01::1".parse().unwrap(),
            pd_endpoints: vec![
                "http://[fd01::1]:2379".into(),
                "http://[fd01::2]:2379".into(),
            ],
        };
        let conf = service::generate_tikv_conf(&cfg);
        assert!(conf.contains("fd01::1"));
        assert!(conf.contains("fd01::2"));
    }

    #[test]
    fn extract_ipv6_works() {
        let ip = extract_ipv6_from_endpoint("http://[fd01::1]:2379");
        assert_eq!(ip, Some("fd01::1".parse().unwrap()));
    }

    #[test]
    fn extract_ipv6_invalid() {
        assert!(extract_ipv6_from_endpoint("http://1.2.3.4:2379").is_none());
    }

    #[test]
    fn max_pd_members_constant() {
        assert_eq!(MAX_PD_MEMBERS, 3);
    }
}
