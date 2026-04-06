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

    // Install configs + systemd units (no join URL for bootstrap)
    service::install(&pd_cfg, &tikv_cfg, None)?;

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
/// 2. Generate configs with PD --join flag pointing to existing cluster
/// 3. Install systemd units
/// 4. Start PD (join mode), wait ready, start TiKV, wait ready
pub fn join(
    node_name: &str,
    mesh_ipv6: &Ipv6Addr,
    existing_pd_endpoints: &[String],
) -> Result<(), SyfrahError> {
    eprintln!("  Joining control plane...");

    // Install binaries
    service::ensure_installed()?;

    let primary_pd = &existing_pd_endpoints[0];

    // PD config — for join, initial-cluster is just self (PD --join handles the rest)
    let self_peer_url = format!(
        "http://[{mesh_ipv6}]:{}",
        super::PD_PEER_PORT
    );

    let pd_cfg = PdConfig {
        name: node_name.to_string(),
        mesh_ipv6: *mesh_ipv6,
        initial_cluster: format!("{node_name}={self_peer_url}"),
        initial_cluster_state: "join".to_string(),
    };

    // TiKV config — all PD endpoints (existing + self)
    let mut pd_endpoints: Vec<String> = existing_pd_endpoints.to_vec();
    let self_endpoint = format!("http://[{mesh_ipv6}]:{}", super::PD_CLIENT_PORT);
    if !pd_endpoints.contains(&self_endpoint) {
        pd_endpoints.push(self_endpoint);
    }

    let tikv_cfg = TikvConfig {
        mesh_ipv6: *mesh_ipv6,
        pd_endpoints,
    };

    // Install configs + systemd units (with --join flag)
    service::install(&pd_cfg, &tikv_cfg, Some(primary_pd))?;

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
        let conf = service::generate_pd_conf(&pd_cfg, false);
        assert!(conf.contains("initial-cluster-state = \"new\""));
        assert!(conf.contains("node-1=http://[fd01::1]:2380"));
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
        // Join mode: no initial-cluster in config
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
