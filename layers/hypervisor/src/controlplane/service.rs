//! TiKV + PD service management — install, configure, start, stop.
//!
//! Same pattern as fabric/service.rs for WireGuard:
//! - Auto-install binaries via TiUP
//! - Generate config files
//! - Install systemd units
//! - Start/stop/status

use std::net::Ipv6Addr;
use std::path::Path;
use std::process::Command;

use syfrah_core::error::SyfrahError;

// ═══════════════════════════════════════════════════
// Paths
// ═══════════════════════════════════════════════════

const TIUP_HOME: &str = "/opt/syfrah/tiup";
const PD_DATA_DIR: &str = "/var/lib/syfrah/pd";
const TIKV_DATA_DIR: &str = "/var/lib/syfrah/tikv";
const PD_CONF_PATH: &str = "/etc/syfrah/pd.toml";
const TIKV_CONF_PATH: &str = "/etc/syfrah/tikv.toml";
const PD_UNIT_PATH: &str = "/etc/systemd/system/syfrah-pd.service";
const TIKV_UNIT_PATH: &str = "/etc/systemd/system/syfrah-tikv.service";

const PD_SERVICE: &str = "syfrah-pd";
const TIKV_SERVICE: &str = "syfrah-tikv";

// ═══════════════════════════════════════════════════
// Install TiUP + components
// ═══════════════════════════════════════════════════

/// Check if TiUP is installed.
fn tiup_available() -> bool {
    Path::new(&format!("{TIUP_HOME}/bin/tiup")).exists()
}

/// Check if PD binary is available.
fn pd_available() -> bool {
    // TiUP installs components under TIUP_HOME/components/pd/...
    Command::new(format!("{TIUP_HOME}/bin/tiup"))
        .args(["list", "--installed"])
        .env("TIUP_HOME", TIUP_HOME)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.contains("pd"))
        .unwrap_or(false)
}

/// Install TiUP and TiKV components.
pub fn ensure_installed() -> Result<(), SyfrahError> {
    // Create dirs
    std::fs::create_dir_all(format!("{TIUP_HOME}/bin")).map_err(SyfrahError::from)?;
    std::fs::create_dir_all("/etc/syfrah").map_err(SyfrahError::from)?;

    if !tiup_available() {
        eprintln!("  Installing TiUP...");

        // Map arch: x86_64→amd64, aarch64→arm64
        let output = Command::new("sh")
            .args(["-c", &format!(
                "ARCH=$(uname -m | sed 's/x86_64/amd64/' | sed 's/aarch64/arm64/') && \
                 curl -fsSL https://tiup-mirrors.pingcap.com/tiup-linux-$ARCH.tar.gz | tar -xz -C {TIUP_HOME}/bin/"
            )])
            .output()
            .map_err(|e| SyfrahError::internal(format!("tiup download failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SyfrahError::internal(format!("tiup install failed: {stderr}")));
        }

        // Initialize TiUP mirror
        let init = Command::new(format!("{TIUP_HOME}/bin/tiup"))
            .args(["mirror", "set", "https://tiup-mirrors.pingcap.com"])
            .env("TIUP_HOME", TIUP_HOME)
            .output()
            .map_err(|e| SyfrahError::internal(format!("tiup mirror set failed: {e}")))?;

        if !init.status.success() {
            let stderr = String::from_utf8_lossy(&init.stderr);
            return Err(SyfrahError::internal(format!("tiup mirror init failed: {stderr}")));
        }
    }

    if !pd_available() {
        eprintln!("  Installing PD + TiKV components...");

        let output = Command::new(format!("{TIUP_HOME}/bin/tiup"))
            .args(["install", "pd", "tikv"])
            .env("TIUP_HOME", TIUP_HOME)
            .output()
            .map_err(|e| SyfrahError::internal(format!("component install failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SyfrahError::internal(format!(
                "PD/TiKV install failed: {stderr}"
            )));
        }

        eprintln!("  PD + TiKV installed");
    }

    Ok(())
}

// ═══════════════════════════════════════════════════
// Config generation
// ═══════════════════════════════════════════════════

/// PD config for a node.
pub struct PdConfig {
    /// This node's name (unique in cluster).
    pub name: String,
    /// Mesh IPv6 address for this node.
    pub mesh_ipv6: Ipv6Addr,
    /// Initial cluster string: "name1=http://[ipv6_1]:2380,name2=http://[ipv6_2]:2380"
    pub initial_cluster: String,
    /// "new" for first node, "join" for subsequent.
    pub initial_cluster_state: String,
}

/// Generate pd.toml config.
/// For join mode, initial-cluster is omitted (--join flag handles it).
pub fn generate_pd_conf(cfg: &PdConfig, is_join: bool) -> String {
    let cluster_section = if is_join {
        // join mode: no initial-cluster in config (--join flag on command line)
        String::new()
    } else {
        format!(
            r#"initial-cluster = "{}"
initial-cluster-state = "new"
"#,
            cfg.initial_cluster
        )
    };

    format!(
        r#"# Syfrah PD configuration — auto-generated
name = "{name}"
data-dir = "{PD_DATA_DIR}"

client-urls = "http://[{ip}]:{client_port}"
peer-urls = "http://[{ip}]:{peer_port}"
advertise-client-urls = "http://[{ip}]:{client_port}"
advertise-peer-urls = "http://[{ip}]:{peer_port}"

{cluster_section}
[log]
level = "warn"

[log.file]
filename = "/var/log/syfrah/pd.log"
max-size = 50
"#,
        name = cfg.name,
        ip = cfg.mesh_ipv6,
        client_port = super::PD_CLIENT_PORT,
        peer_port = super::PD_PEER_PORT,
    )
}

/// TiKV config for a node.
pub struct TikvConfig {
    /// Mesh IPv6 address.
    pub mesh_ipv6: Ipv6Addr,
    /// PD endpoints: "http://[ipv6_1]:2379,http://[ipv6_2]:2379"
    pub pd_endpoints: Vec<String>,
}

/// Generate tikv.toml config.
pub fn generate_tikv_conf(cfg: &TikvConfig) -> String {
    let pd_endpoints: Vec<String> = cfg
        .pd_endpoints
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect();

    format!(
        r#"# Syfrah TiKV configuration — auto-generated
[server]
addr = "[{ip}]:{tikv_port}"
advertise-addr = "[{ip}]:{tikv_port}"

[storage]
data-dir = "{TIKV_DATA_DIR}"

[pd]
endpoints = [{pd_list}]

[log]
level = "warn"

[log.file]
filename = "/var/log/syfrah/tikv.log"
max-size = 50

[raftstore]
# Reduce resource usage for small clusters
capacity = "0"
"#,
        ip = cfg.mesh_ipv6,
        tikv_port = super::TIKV_PORT,
        pd_list = pd_endpoints.join(", "),
    )
}

// ═══════════════════════════════════════════════════
// Systemd units
// ═══════════════════════════════════════════════════

fn generate_pd_unit(join_url: Option<&str>) -> String {
    let exec_start = match join_url {
        Some(url) => format!(
            "ExecStart={TIUP_HOME}/bin/tiup pd --config={PD_CONF_PATH} --join={url}"
        ),
        None => format!(
            "ExecStart={TIUP_HOME}/bin/tiup pd --config={PD_CONF_PATH}"
        ),
    };

    format!(
        r#"[Unit]
Description=Syfrah Placement Driver (PD)
After=network-online.target syfrah-wg.service
Wants=network-online.target
Requires=syfrah-wg.service

[Service]
Type=simple
Environment=HOME=/root
Environment=TIUP_HOME={TIUP_HOME}
{exec_start}
Restart=on-failure
RestartSec=5
LimitNOFILE=1000000

[Install]
WantedBy=multi-user.target
"#
    )
}

fn generate_tikv_unit() -> String {
    format!(
        r#"[Unit]
Description=Syfrah TiKV Storage Engine
After=network-online.target syfrah-wg.service
Wants=network-online.target
Requires=syfrah-wg.service

[Service]
Type=simple
Environment=HOME=/root
Environment=TIUP_HOME={TIUP_HOME}
ExecStart={TIUP_HOME}/bin/tiup tikv --config={TIKV_CONF_PATH}
Restart=on-failure
RestartSec=5
LimitNOFILE=1000000

[Install]
WantedBy=multi-user.target
"#
    )
}

// ═══════════════════════════════════════════════════
// Install, start, stop
// ═══════════════════════════════════════════════════

/// Install PD + TiKV configs and systemd units.
/// `join_url` is Some for joining an existing cluster.
pub fn install(pd_cfg: &PdConfig, tikv_cfg: &TikvConfig, join_url: Option<&str>) -> Result<(), SyfrahError> {
    // Create directories
    std::fs::create_dir_all(PD_DATA_DIR).map_err(SyfrahError::from)?;
    std::fs::create_dir_all(TIKV_DATA_DIR).map_err(SyfrahError::from)?;
    std::fs::create_dir_all("/var/log/syfrah").map_err(SyfrahError::from)?;
    std::fs::create_dir_all("/etc/syfrah").map_err(SyfrahError::from)?;

    // Write configs
    let is_join = join_url.is_some();
    std::fs::write(PD_CONF_PATH, generate_pd_conf(pd_cfg, is_join)).map_err(SyfrahError::from)?;
    std::fs::write(TIKV_CONF_PATH, generate_tikv_conf(tikv_cfg)).map_err(SyfrahError::from)?;

    // Write systemd units
    std::fs::write(PD_UNIT_PATH, generate_pd_unit(join_url)).map_err(SyfrahError::from)?;
    std::fs::write(TIKV_UNIT_PATH, generate_tikv_unit()).map_err(SyfrahError::from)?;

    // Reload systemd
    run_systemctl(&["daemon-reload"])?;

    Ok(())
}

/// Install TiKV only (no PD) — for nodes beyond the PD member limit.
pub fn install_tikv_only(tikv_cfg: &TikvConfig) -> Result<(), SyfrahError> {
    std::fs::create_dir_all(TIKV_DATA_DIR).map_err(SyfrahError::from)?;
    std::fs::create_dir_all("/var/log/syfrah").map_err(SyfrahError::from)?;
    std::fs::create_dir_all("/etc/syfrah").map_err(SyfrahError::from)?;

    std::fs::write(TIKV_CONF_PATH, generate_tikv_conf(tikv_cfg)).map_err(SyfrahError::from)?;
    std::fs::write(TIKV_UNIT_PATH, generate_tikv_unit()).map_err(SyfrahError::from)?;

    run_systemctl(&["daemon-reload"])?;
    Ok(())
}

/// Start TiKV only (no PD).
pub fn start_tikv_only() -> Result<(), SyfrahError> {
    run_systemctl(&["enable", "--now", TIKV_SERVICE])
}

/// Enable and start PD, then TiKV (order matters).
pub fn enable_and_start() -> Result<(), SyfrahError> {
    run_systemctl(&["enable", "--now", PD_SERVICE])?;
    // Wait for PD to be ready before starting TiKV
    std::thread::sleep(std::time::Duration::from_secs(3));
    run_systemctl(&["enable", "--now", TIKV_SERVICE])?;
    Ok(())
}

/// Start both services.
pub fn start() -> Result<(), SyfrahError> {
    run_systemctl(&["start", PD_SERVICE])?;
    std::thread::sleep(std::time::Duration::from_secs(2));
    run_systemctl(&["start", TIKV_SERVICE])?;
    Ok(())
}

/// Stop both services.
pub fn stop() -> Result<(), SyfrahError> {
    let _ = run_systemctl(&["stop", TIKV_SERVICE]);
    let _ = run_systemctl(&["stop", PD_SERVICE]);
    Ok(())
}

/// Check if PD is active.
pub fn pd_is_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", PD_SERVICE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if TiKV is active.
pub fn tikv_is_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", TIKV_SERVICE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if both are installed.
pub fn is_installed() -> bool {
    Path::new(PD_UNIT_PATH).exists() && Path::new(TIKV_UNIT_PATH).exists()
}

/// Uninstall everything — stop services, remove configs, data.
pub fn uninstall() -> Result<(), SyfrahError> {
    let _ = run_systemctl(&["disable", "--now", TIKV_SERVICE]);
    let _ = run_systemctl(&["disable", "--now", PD_SERVICE]);

    let _ = std::fs::remove_file(PD_UNIT_PATH);
    let _ = std::fs::remove_file(TIKV_UNIT_PATH);
    let _ = std::fs::remove_file(PD_CONF_PATH);
    let _ = std::fs::remove_file(TIKV_CONF_PATH);
    let _ = std::fs::remove_dir_all(PD_DATA_DIR);
    let _ = std::fs::remove_dir_all(TIKV_DATA_DIR);

    let _ = run_systemctl(&["daemon-reload"]);

    Ok(())
}

/// Reload configs without full restart (PD hot-reload + TiKV syncconf).
pub fn reload(pd_cfg: &PdConfig, tikv_cfg: &TikvConfig) -> Result<(), SyfrahError> {
    // Rewrite configs
    std::fs::write(PD_CONF_PATH, generate_pd_conf(pd_cfg, false)).map_err(SyfrahError::from)?;
    std::fs::write(TIKV_CONF_PATH, generate_tikv_conf(tikv_cfg)).map_err(SyfrahError::from)?;

    // PD supports SIGHUP for config reload
    if pd_is_active() {
        let _ = Command::new("systemctl").args(["reload-or-restart", PD_SERVICE]).output();
    }
    // TiKV needs restart for config changes
    if tikv_is_active() {
        let _ = run_systemctl(&["restart", TIKV_SERVICE]);
    }

    Ok(())
}

/// Restart both services (stop then start, ordered).
pub fn restart() -> Result<(), SyfrahError> {
    stop()?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    start()
}

/// Wait for PD to be healthy (responds on client URL).
pub fn wait_pd_ready(mesh_ipv6: &Ipv6Addr, timeout_secs: u64) -> Result<(), SyfrahError> {
    let url = format!("http://[{}]:{}/pd/api/v1/health", mesh_ipv6, super::PD_CLIENT_PORT);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    while std::time::Instant::now() < deadline {
        let result = Command::new("curl")
            .args(["-sf", "--max-time", "2", &url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if result.map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    Err(SyfrahError::timeout("PD health check", timeout_secs))
}

/// Wait for TiKV to register with PD (at least 1 store).
pub fn wait_tikv_ready(mesh_ipv6: &Ipv6Addr, timeout_secs: u64) -> Result<(), SyfrahError> {
    let url = format!(
        "http://[{}]:{}/pd/api/v1/stores",
        mesh_ipv6,
        super::PD_CLIENT_PORT
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    while std::time::Instant::now() < deadline {
        if let Ok(output) = Command::new("curl")
            .args(["-sf", "--max-time", "2", &url])
            .output()
        {
            if output.status.success() {
                let body = String::from_utf8_lossy(&output.stdout);
                // PD returns {"count": N, "stores": [...]}
                if body.contains("\"count\"") && !body.contains("\"count\":0") {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    Err(SyfrahError::timeout("TiKV store registration", timeout_secs))
}

/// Get cluster status from PD API.
pub fn cluster_status(mesh_ipv6: &Ipv6Addr) -> Result<ClusterStatus, SyfrahError> {
    let pd_url = format!(
        "http://[{}]:{}",
        mesh_ipv6,
        super::PD_CLIENT_PORT
    );

    let members = pd_api_get(&pd_url, "/pd/api/v1/members");
    let stores = pd_api_get(&pd_url, "/pd/api/v1/stores");

    let member_count = members
        .as_ref()
        .ok()
        .and_then(|v| v["members"].as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let store_count = stores
        .as_ref()
        .ok()
        .and_then(|v| v["count"].as_u64())
        .unwrap_or(0) as usize;

    let leader = members
        .as_ref()
        .ok()
        .and_then(|v| v["leader"]["name"].as_str())
        .map(|s| s.to_string());

    Ok(ClusterStatus {
        pd_active: pd_is_active(),
        tikv_active: tikv_is_active(),
        pd_members: member_count,
        tikv_stores: store_count,
        leader,
    })
}

/// Cluster health status.
#[derive(Debug, Clone)]
pub struct ClusterStatus {
    pub pd_active: bool,
    pub tikv_active: bool,
    pub pd_members: usize,
    pub tikv_stores: usize,
    pub leader: Option<String>,
}

/// Query PD HTTP API.
fn pd_api_get(pd_url: &str, path: &str) -> Result<serde_json::Value, SyfrahError> {
    let url = format!("{pd_url}{path}");
    let output = Command::new("curl")
        .args(["-sf", "--max-time", "5", &url])
        .output()
        .map_err(|e| SyfrahError::internal(format!("curl failed: {e}")))?;

    if !output.status.success() {
        return Err(SyfrahError::internal("PD API request failed"));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|e| SyfrahError::internal(format!("PD API parse failed: {e}")))
}

fn run_systemctl(args: &[&str]) -> Result<(), SyfrahError> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| SyfrahError::internal(format!("systemctl failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SyfrahError::internal(format!(
            "systemctl {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_pd_conf_single_node() {
        let cfg = PdConfig {
            name: "node-1".into(),
            mesh_ipv6: "fd01::1".parse().unwrap(),
            initial_cluster: "node-1=http://[fd01::1]:2380".into(),
            initial_cluster_state: "new".into(),
        };
        let conf = generate_pd_conf(&cfg, false);
        assert!(conf.contains("name = \"node-1\""));
        assert!(conf.contains("[fd01::1]:2379"));
        assert!(conf.contains("[fd01::1]:2380"));
        assert!(conf.contains("initial-cluster-state = \"new\""));
    }

    #[test]
    fn generate_pd_conf_join_mode() {
        let cfg = PdConfig {
            name: "node-2".into(),
            mesh_ipv6: "fd01::2".parse().unwrap(),
            initial_cluster: "node-2=http://[fd01::2]:2380".into(),
            initial_cluster_state: "join".into(),
        };
        // In join mode, initial-cluster is omitted from config (--join flag handles it)
        let conf = generate_pd_conf(&cfg, true);
        assert!(!conf.contains("initial-cluster"));
        assert!(conf.contains("name = \"node-2\""));
        assert!(conf.contains("[fd01::2]:2379"));
    }

    #[test]
    fn generate_tikv_conf_basic() {
        let cfg = TikvConfig {
            mesh_ipv6: "fd01::1".parse().unwrap(),
            pd_endpoints: vec!["http://[fd01::1]:2379".into()],
        };
        let conf = generate_tikv_conf(&cfg);
        assert!(conf.contains("[fd01::1]:20160"));
        assert!(conf.contains("http://[fd01::1]:2379"));
    }

    #[test]
    fn generate_tikv_conf_multi_pd() {
        let cfg = TikvConfig {
            mesh_ipv6: "fd01::1".parse().unwrap(),
            pd_endpoints: vec![
                "http://[fd01::1]:2379".into(),
                "http://[fd01::2]:2379".into(),
                "http://[fd01::3]:2379".into(),
            ],
        };
        let conf = generate_tikv_conf(&cfg);
        assert!(conf.contains("fd01::2"));
        assert!(conf.contains("fd01::3"));
    }

    #[test]
    fn generate_pd_unit_valid() {
        let unit = generate_pd_unit(None);
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("syfrah-wg.service"));
        assert!(unit.contains("pd --config="));
    }

    #[test]
    fn generate_tikv_unit_valid() {
        let unit = generate_tikv_unit();
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("syfrah-wg.service"));
        assert!(unit.contains("tikv --config="));
    }

    #[test]
    fn is_installed_false_by_default() {
        // On test system without syfrah
        assert!(!is_installed());
    }
}
