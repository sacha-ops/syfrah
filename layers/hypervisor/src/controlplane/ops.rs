//! Control plane operations — high-level orchestration.
//!
//! Called by the hypervisor handler during init/join/leave/status.
//! Orchestrates TiUP install → config → systemd → health check.

use std::net::Ipv6Addr;

use syfrah_core::error::SyfrahError;

use super::service::{self, PdConfig, TikvConfig};

/// Bootstrap a new single-node TiKV cluster.
///
/// Called during `hypervisor init`:
/// 1. Install TiUP + PD + TiKV binaries
/// 2. Generate configs for single-node cluster
/// 3. Install systemd units
/// 4. Start PD, wait ready, start TiKV, wait ready
pub fn bootstrap(
    node_name: &str,
    mesh_ipv6: &Ipv6Addr,
) -> Result<(), SyfrahError> {
    eprintln!("  Setting up control plane...");

    // Install binaries
    service::ensure_installed()?;

    // Single-node PD config
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

    // Install configs + systemd units
    service::install(&pd_cfg, &tikv_cfg)?;

    // Start and wait for readiness
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
/// Called during `hypervisor join`:
/// 1. Install TiUP + PD + TiKV binaries
/// 2. Add self as PD member via existing PD's API
/// 3. Generate configs pointing to all known PD endpoints
/// 4. Install systemd units
/// 5. Start PD (join mode), wait ready, start TiKV, wait ready
pub fn join(
    node_name: &str,
    mesh_ipv6: &Ipv6Addr,
    existing_pd_endpoints: &[String],
) -> Result<(), SyfrahError> {
    eprintln!("  Joining control plane...");

    // Install binaries
    service::ensure_installed()?;

    // Register with existing PD cluster
    let primary_pd = &existing_pd_endpoints[0];
    let new_peer_url = format!(
        "http://[{mesh_ipv6}]:{}",
        super::PD_PEER_PORT
    );
    service::add_pd_member(primary_pd, node_name, &new_peer_url)?;

    // Build initial-cluster string with all members + self
    let mut initial_cluster_parts: Vec<String> = Vec::new();
    // Get existing members from PD API
    if let Ok(members) = get_pd_members(primary_pd) {
        for (name, peer_url) in &members {
            initial_cluster_parts.push(format!("{name}={peer_url}"));
        }
    }
    // Add ourselves
    initial_cluster_parts.push(format!("{node_name}={new_peer_url}"));
    let initial_cluster = initial_cluster_parts.join(",");

    // All PD endpoints (existing + self)
    let mut pd_endpoints: Vec<String> = existing_pd_endpoints.to_vec();
    let self_endpoint = format!("http://[{mesh_ipv6}]:{}", super::PD_CLIENT_PORT);
    if !pd_endpoints.contains(&self_endpoint) {
        pd_endpoints.push(self_endpoint);
    }

    let pd_cfg = PdConfig {
        name: node_name.to_string(),
        mesh_ipv6: *mesh_ipv6,
        initial_cluster,
        initial_cluster_state: "join".to_string(),
    };

    let tikv_cfg = TikvConfig {
        mesh_ipv6: *mesh_ipv6,
        pd_endpoints,
    };

    // Install configs + systemd units
    service::install(&pd_cfg, &tikv_cfg)?;

    // Start and wait
    service::enable_and_start()?;
    eprintln!("  Waiting for PD...");
    service::wait_pd_ready(mesh_ipv6, 30)?;
    eprintln!("  Waiting for TiKV...");
    service::wait_tikv_ready(mesh_ipv6, 60)?;

    eprintln!("  Control plane joined");
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

/// Uninstall the control plane — stop services, remove data.
pub fn leave() -> Result<(), SyfrahError> {
    service::uninstall()
}

/// Get PD member list from the PD API: Vec<(name, peer_url)>.
fn get_pd_members(pd_url: &str) -> Result<Vec<(String, String)>, SyfrahError> {
    let url = format!("{pd_url}/pd/api/v1/members");
    let output = std::process::Command::new("curl")
        .args(["-sf", "--max-time", "5", &url])
        .output()
        .map_err(|e| SyfrahError::internal(format!("curl failed: {e}")))?;

    if !output.status.success() {
        return Err(SyfrahError::internal("failed to get PD members"));
    }

    let body: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| SyfrahError::internal(format!("parse PD members: {e}")))?;

    let mut members = Vec::new();
    if let Some(arr) = body["members"].as_array() {
        for m in arr {
            let name = m["name"].as_str().unwrap_or("").to_string();
            let peer_url = m["peer_urls"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() && !peer_url.is_empty() {
                members.push((name, peer_url));
            }
        }
    }

    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_config_single_node() {
        let ip: Ipv6Addr = "fd01::1".parse().unwrap();
        let pd_cfg = PdConfig {
            name: "node-1".into(),
            mesh_ipv6: ip,
            initial_cluster: format!("node-1=http://[{ip}]:2380"),
            initial_cluster_state: "new".into(),
        };
        let conf = service::generate_pd_conf(&pd_cfg);
        assert!(conf.contains("initial-cluster-state = \"new\""));
        assert!(conf.contains("node-1=http://[fd01::1]:2380"));
    }

    #[test]
    fn join_config_format() {
        let ip: Ipv6Addr = "fd01::2".parse().unwrap();
        let pd_cfg = PdConfig {
            name: "node-2".into(),
            mesh_ipv6: ip,
            initial_cluster: "node-1=http://[fd01::1]:2380,node-2=http://[fd01::2]:2380".into(),
            initial_cluster_state: "join".into(),
        };
        let conf = service::generate_pd_conf(&pd_cfg);
        assert!(conf.contains("initial-cluster-state = \"join\""));
        assert!(conf.contains("node-1"));
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
    fn cluster_status_when_not_running() {
        // On a test system, nothing is running
        let status = service::cluster_status(&"fd01::1".parse().unwrap());
        // Should not panic — just returns inactive
        if let Ok(s) = status {
            assert!(!s.pd_active);
            assert!(!s.tikv_active);
        }
    }
}
