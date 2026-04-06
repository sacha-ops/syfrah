//! Hypervisor doctor — diagnose the health of all sub-systems.
//!
//! Runs sequential checks against fabric, controlplane, storage, and system.
//! Outputs a human-readable report with OK / WARN / FAIL for each check.

use std::net::Ipv6Addr;
use std::path::Path;
use std::process::Command;

use crate::controlplane;
use crate::fabric;
use crate::storage;

// ═══════════════════════════════════════════════════
// Check result types
// ═══════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skip,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct DoctorReport {
    pub checks: Vec<(String, Vec<Check>)>, // (section_name, checks)
}

impl DoctorReport {
    fn add_section(&mut self, name: &str) -> &mut Vec<Check> {
        self.checks.push((name.to_string(), Vec::new()));
        &mut self.checks.last_mut().unwrap().1
    }

    pub fn ok_count(&self) -> usize {
        self.checks.iter().flat_map(|(_, c)| c).filter(|c| c.status == CheckStatus::Ok).count()
    }

    pub fn warn_count(&self) -> usize {
        self.checks.iter().flat_map(|(_, c)| c).filter(|c| c.status == CheckStatus::Warn).count()
    }

    pub fn fail_count(&self) -> usize {
        self.checks.iter().flat_map(|(_, c)| c).filter(|c| c.status == CheckStatus::Fail).count()
    }

    /// Render the report to stderr.
    pub fn print(&self) {
        for (section, checks) in &self.checks {
            eprintln!();
            eprintln!("  {section}");
            for check in checks {
                let icon = match check.status {
                    CheckStatus::Ok => "\x1b[32m✓\x1b[0m",
                    CheckStatus::Warn => "\x1b[33m!\x1b[0m",
                    CheckStatus::Fail => "\x1b[31m✗\x1b[0m",
                    CheckStatus::Skip => "\x1b[90m-\x1b[0m",
                };
                eprintln!("    {icon} {}: {}", check.name, check.detail);
            }
        }
        eprintln!();
        eprintln!(
            "  {} passed, {} warnings, {} errors",
            self.ok_count(),
            self.warn_count(),
            self.fail_count()
        );
    }
}

fn ok(name: &str, detail: &str) -> Check {
    Check { name: name.into(), status: CheckStatus::Ok, detail: detail.into() }
}

fn warn(name: &str, detail: &str) -> Check {
    Check { name: name.into(), status: CheckStatus::Warn, detail: detail.into() }
}

fn fail(name: &str, detail: &str) -> Check {
    Check { name: name.into(), status: CheckStatus::Fail, detail: detail.into() }
}

fn skip(name: &str, detail: &str) -> Check {
    Check { name: name.into(), status: CheckStatus::Skip, detail: detail.into() }
}

// ═══════════════════════════════════════════════════
// Run all checks
// ═══════════════════════════════════════════════════

pub fn run() -> DoctorReport {
    let mut report = DoctorReport::default();

    // Load state
    let db = syfrah_state::LocalDb::open("hypervisor").ok();
    let state = db
        .as_ref()
        .and_then(|db| fabric::state::FabricState::load(db).ok().flatten());

    let mesh_ipv6 = state.as_ref().map(|s| s.hypervisor.mesh_ipv6);

    check_fabric(&mut report, &state);
    check_controlplane(&mut report, mesh_ipv6.as_ref());
    check_storage(&mut report, db.as_ref());
    check_system(&mut report);

    report
}

// ═══════════════════════════════════════════════════
// Fabric checks
// ═══════════════════════════════════════════════════

fn check_fabric(report: &mut DoctorReport, state: &Option<fabric::state::FabricState>) {
    let checks = report.add_section("Fabric");

    // wireguard-tools installed
    if cmd_exists("wg") {
        checks.push(ok("wireguard-tools", "installed"));
    } else {
        checks.push(fail("wireguard-tools", "not installed"));
    }

    // Interface up
    if fabric::wg::interface_exists() {
        checks.push(ok("syfrah0 interface", "up"));
    } else {
        checks.push(fail("syfrah0 interface", "down"));
    }

    // Systemd service
    if fabric::service::is_active() {
        checks.push(ok("wg service", "active"));
    } else if fabric::service::is_installed() {
        checks.push(warn("wg service", "installed but stopped"));
    } else {
        checks.push(fail("wg service", "not installed"));
    }

    // Config file
    if Path::new("/etc/wireguard/syfrah0.conf").exists() {
        checks.push(ok("wg config", "/etc/wireguard/syfrah0.conf exists"));
    } else {
        checks.push(fail("wg config", "missing"));
    }

    // Peer health
    if let Some(state) = state {
        let total = state.peers.len();
        if total == 0 {
            checks.push(ok("peers", "no peers configured"));
        } else {
            let active = state.peers.active_count();
            let unreachable = state.peers.unreachable_count();
            if unreachable == 0 {
                checks.push(ok("peers", &format!("{total}/{total} reachable")));
            } else {
                checks.push(warn(
                    "peers",
                    &format!("{active}/{total} reachable, {unreachable} unreachable"),
                ));
            }

            // Ping test (sample first 3 peers)
            let mut ping_ok = 0;
            let mut ping_fail = 0;
            for peer in state.peers.peers.iter().take(3) {
                if ping6(&peer.mesh_ipv6) {
                    ping_ok += 1;
                } else {
                    ping_fail += 1;
                }
            }
            if ping_fail == 0 {
                checks.push(ok("mesh ping", &format!("{ping_ok}/{ping_ok} responded")));
            } else {
                checks.push(warn(
                    "mesh ping",
                    &format!("{ping_ok}/{} responded", ping_ok + ping_fail),
                ));
            }
        }
    } else {
        checks.push(skip("peers", "not initialized"));
    }
}

// ═══════════════════════════════════════════════════
// Controlplane checks
// ═══════════════════════════════════════════════════

fn check_controlplane(report: &mut DoctorReport, mesh_ipv6: Option<&Ipv6Addr>) {
    let checks = report.add_section("Controlplane");

    // TiUP installed
    if Path::new("/opt/syfrah/tiup/bin/tiup").exists() {
        checks.push(ok("tiup", "installed"));
    } else {
        checks.push(skip("tiup", "not installed"));
        return;
    }

    // PD service
    if controlplane::service::pd_is_active() {
        checks.push(ok("pd service", "active"));
    } else if Path::new("/etc/systemd/system/syfrah-pd.service").exists() {
        checks.push(warn("pd service", "installed but stopped"));
    } else {
        checks.push(ok("pd service", "not running (tikv-only node)"));
    }

    // TiKV service
    if controlplane::service::tikv_is_active() {
        checks.push(ok("tikv service", "active"));
    } else if Path::new("/etc/systemd/system/syfrah-tikv.service").exists() {
        checks.push(fail("tikv service", "installed but stopped"));
    } else {
        checks.push(fail("tikv service", "not installed"));
    }

    // PD health + leader
    if let Some(ip) = mesh_ipv6 {
        let pd_url = format!("http://[{}]:{}", ip, controlplane::PD_CLIENT_PORT);
        let health_url = format!("{pd_url}/pd/api/v1/health");

        if let Some(body) = curl_json(&health_url) {
            if let Some(members) = body.as_array() {
                let healthy: Vec<_> = members
                    .iter()
                    .filter(|m| m["health"].as_bool().unwrap_or(false))
                    .collect();
                let total = members.len();
                if healthy.len() == total {
                    checks.push(ok("pd health", &format!("{total}/{total} healthy")));
                } else {
                    checks.push(warn(
                        "pd health",
                        &format!("{}/{total} healthy", healthy.len()),
                    ));
                }

                // Leader
                let leader = members
                    .iter()
                    .find(|m| m["health"].as_bool().unwrap_or(false))
                    .and_then(|m| m["name"].as_str());
                if let Some(name) = leader {
                    checks.push(ok("pd leader", name));
                } else {
                    checks.push(fail("pd leader", "no leader elected"));
                }
            }
        } else {
            checks.push(warn("pd health", "could not reach PD API"));
        }

        // TiKV store registration
        let stores_url = format!("{pd_url}/pd/api/v1/stores");
        if let Some(body) = curl_json(&stores_url) {
            let count = body["count"].as_u64().unwrap_or(0);
            checks.push(ok("tikv stores", &format!("{count} registered")));
        }
    }
}

// ═══════════════════════════════════════════════════
// Storage checks
// ═══════════════════════════════════════════════════

fn check_storage(report: &mut DoctorReport, db: Option<&syfrah_state::LocalDb>) {
    let checks = report.add_section("Storage");

    // ZeroFS installed
    if storage::service::is_installed() {
        checks.push(ok("zerofs", "installed"));
    } else {
        checks.push(skip("zerofs", "not installed"));
    }

    // Region configs
    if let Some(db) = db {
        let statuses = storage::ops::status(db);
        if statuses.is_empty() {
            checks.push(ok("regions", "none configured"));
        } else {
            for s in &statuses {
                if s.active {
                    checks.push(ok(
                        &format!("region {}", s.region),
                        &format!("active ({})", s.s3_bucket),
                    ));
                } else {
                    checks.push(warn(
                        &format!("region {}", s.region),
                        &format!("stopped ({})", s.s3_bucket),
                    ));
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════
// System checks
// ═══════════════════════════════════════════════════

fn check_system(report: &mut DoctorReport) {
    let checks = report.add_section("System");

    // Disk space
    if let Some(free_mb) = disk_free_mb("/var/lib/syfrah") {
        if free_mb > 1024 {
            checks.push(ok("disk space", &format!("{}GB free", free_mb / 1024)));
        } else if free_mb > 256 {
            checks.push(warn("disk space", &format!("{free_mb}MB free (low)")));
        } else {
            checks.push(fail("disk space", &format!("{free_mb}MB free (critical)")));
        }
    }

    // Log directory
    if Path::new("/var/log/syfrah").exists() {
        let log_file = Path::new("/var/log/syfrah/syfrah.log");
        if log_file.exists() || std::fs::File::create("/var/log/syfrah/.doctor-test").is_ok() {
            let _ = std::fs::remove_file("/var/log/syfrah/.doctor-test");
            checks.push(ok("logs", "/var/log/syfrah writable"));
        } else {
            checks.push(warn("logs", "/var/log/syfrah not writable"));
        }
    } else {
        checks.push(warn("logs", "/var/log/syfrah missing"));
    }

    // State file
    let state_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/root"))
        .join(".syfrah/hypervisor.json");
    if state_path.exists() {
        match std::fs::read_to_string(&state_path) {
            Ok(content) => {
                if serde_json::from_str::<serde_json::Value>(&content).is_ok() {
                    checks.push(ok("state file", "valid JSON"));
                } else {
                    checks.push(fail("state file", "corrupt (invalid JSON)"));
                }
            }
            Err(e) => checks.push(fail("state file", &format!("unreadable: {e}"))),
        }
    } else {
        checks.push(skip("state file", "not initialized"));
    }
}

// ═══════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════

fn cmd_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ping6(addr: &Ipv6Addr) -> bool {
    Command::new("ping6")
        .args(["-c", "1", "-W", "2", &addr.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn curl_json(url: &str) -> Option<serde_json::Value> {
    let output = Command::new("curl")
        .args(["-sf", "--max-time", "5", url])
        .output()
        .ok()?;
    if output.status.success() {
        serde_json::from_slice(&output.stdout).ok()
    } else {
        None
    }
}

fn disk_free_mb(path: &str) -> Option<u64> {
    let output = Command::new("df")
        .args(["-m", "--output=avail", path])
        .output()
        .ok()?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .last()
            .and_then(|l| l.trim().parse::<u64>().ok())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_counts() {
        let mut report = DoctorReport::default();
        let checks = report.add_section("Test");
        checks.push(ok("a", "good"));
        checks.push(ok("b", "good"));
        checks.push(warn("c", "meh"));
        checks.push(fail("d", "bad"));

        assert_eq!(report.ok_count(), 2);
        assert_eq!(report.warn_count(), 1);
        assert_eq!(report.fail_count(), 1);
    }

    #[test]
    fn check_constructors() {
        let c = ok("test", "detail");
        assert_eq!(c.status, CheckStatus::Ok);
        assert_eq!(c.name, "test");

        let c = warn("test", "detail");
        assert_eq!(c.status, CheckStatus::Warn);

        let c = fail("test", "detail");
        assert_eq!(c.status, CheckStatus::Fail);

        let c = skip("test", "detail");
        assert_eq!(c.status, CheckStatus::Skip);
    }

    #[test]
    fn cmd_exists_check() {
        assert!(cmd_exists("ls"));
        assert!(!cmd_exists("nonexistent_command_xyz"));
    }

    #[test]
    fn disk_free_check() {
        // / should always have some free space
        let free = disk_free_mb("/");
        assert!(free.is_some());
        assert!(free.unwrap() > 0);
    }

    #[test]
    fn run_doctor_no_panic() {
        // On a test system, doctor should run without panic
        let report = run();
        // Should have 4 sections
        assert_eq!(report.checks.len(), 4);
    }
}
