use crate::{
    audit, config, events::ZoneHealthStatus, sd_watchdog, store, topology::TopologyView, ui, wg,
};
use anyhow::Result;
use serde::Serialize;
use syfrah_core::mesh::{PeerStatus, Zone};
use syfrah_state::LayerDb;

#[derive(Serialize)]
struct DiagnoseCheck {
    name: String,
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct DiagnoseOutput {
    checks: Vec<DiagnoseCheck>,
    passed: u32,
    failed: u32,
    total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    zone_diagnosis: Option<ZoneDiagnosis>,
}

#[derive(Serialize)]
struct ZoneDiagnosis {
    zone: String,
    total_nodes: usize,
    active_nodes: usize,
    health: String,
    nodes: Vec<ZoneNodeStatus>,
    possible_causes: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct ZoneNodeStatus {
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_handshake_ago: Option<String>,
}

/// Format a duration in seconds as a human-readable "ago" string.
fn fmt_ago(epoch_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if epoch_secs > now {
        return "just now".to_string();
    }
    let delta = now - epoch_secs;
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86400)
    }
}

pub async fn run(json: bool, zone_filter: Option<String>) -> Result<()> {
    let tuning = config::load_tuning().unwrap_or_default();
    wg::set_interface_name(&tuning.interface_name);

    let mut checks: Vec<DiagnoseCheck> = Vec::new();
    let mut pass_count = 0u32;
    let mut fail_count = 0u32;

    macro_rules! check {
        ($name:expr, $result:expr, $detail:expr) => {{
            let name: String = $name.into();
            let result: bool = $result;
            let detail: &str = $detail;
            if result {
                pass_count += 1;
            } else {
                fail_count += 1;
            }
            if !json {
                if result {
                    ui::check_pass(&name);
                } else {
                    ui::check_fail(&name, detail);
                }
            }
            checks.push(DiagnoseCheck {
                name,
                passed: result,
                detail: if detail.is_empty() {
                    None
                } else {
                    Some(detail.to_string())
                },
            });
        }};
    }

    if !json {
        ui::heading("Syfrah Fabric Diagnostics");
        println!();
    }

    // -- State store --
    if !json {
        ui::heading("State store");
    }
    let state_exists = store::exists();
    check!(
        "Mesh state exists",
        state_exists,
        "run 'syfrah fabric init' or 'syfrah fabric join'"
    );

    let state_file = dirs::home_dir()
        .unwrap_or_default()
        .join(".syfrah")
        .join("state.json");
    let json_ok = if state_file.exists() {
        std::fs::read_to_string(&state_file)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some()
    } else {
        false
    };
    if state_file.exists() {
        check!("state.json is valid JSON", json_ok, "file may be corrupted");
    }

    let redb_ok = LayerDb::layer_exists("fabric") && LayerDb::open("fabric").is_ok();
    check!(
        "redb database is readable",
        redb_ok || !LayerDb::layer_exists("fabric"),
        "fabric.redb may be corrupted"
    );

    let state = store::load();
    if let Ok(ref s) = state {
        check!(
            format!("Loaded {} peers from state", s.peers.len()),
            true,
            ""
        );
    }
    if !json {
        println!();
    }

    // -- Daemon --
    if !json {
        ui::heading("Daemon");
    }
    let pid = store::daemon_running();
    check!(
        "Daemon process",
        pid.is_some(),
        "daemon is not running — start with 'syfrah fabric start'"
    );

    let socket_path = store::control_socket_path();
    let socket_exists = socket_path.exists();
    check!(
        "Control socket exists",
        socket_exists,
        &format!("missing: {}", socket_path.display())
    );

    let audit_path = audit::audit_log_path();
    let audit_ok = if audit_path.exists() {
        let writable = std::fs::OpenOptions::new()
            .append(true)
            .open(&audit_path)
            .is_ok();
        let size = std::fs::metadata(&audit_path).map(|m| m.len()).unwrap_or(0);
        check!(
            format!("Audit log exists ({size} bytes)"),
            writable,
            "audit log is not writable"
        );
        writable
    } else {
        // Not an error if the log doesn't exist yet (first run).
        check!("Audit log", true, "");
        true
    };
    let _ = audit_ok;

    let log_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".syfrah")
        .join("syfrah.log");
    if log_path.exists() {
        let log_size = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
        check!(format!("Log file ({} bytes)", log_size), true, "");
    } else {
        check!("Log file", false, "~/.syfrah/syfrah.log not found");
    }
    if !json {
        println!();
    }

    // -- WireGuard --
    if !json {
        ui::heading("WireGuard");
    }
    let wg_summary = wg::interface_summary();
    match wg_summary {
        Ok(ref summary) => {
            check!(
                format!("Interface {} is up", wg::interface_name()),
                true,
                ""
            );
            check!(
                format!(
                    "{} WG peers configured, {} with handshake",
                    summary.peer_count,
                    summary
                        .peers
                        .iter()
                        .filter(|p| p.last_handshake.is_some())
                        .count()
                ),
                true,
                ""
            );

            // Check consistency: stored peers vs WG peers
            if let Ok(ref s) = state {
                let stored_count = s.peers.len();
                let wg_count = summary.peer_count;
                let consistent = stored_count == wg_count;
                check!(
                    format!("Store/WG consistency ({stored_count} stored, {wg_count} in WG)"),
                    consistent,
                    "mismatch — reconciliation may fix this"
                );
            }
        }
        Err(ref e) => {
            check!(
                format!("Interface {}", wg::interface_name()),
                false,
                &format!("not found: {e}")
            );
        }
    }

    // -- Systemd integration --
    if !json {
        println!();
        ui::heading("Systemd");
    }
    let unit_installed = std::path::Path::new(crate::cli::service::UNIT_FILE_PATH).exists();
    check!(
        "Unit file installed",
        unit_installed,
        "run 'syfrah fabric service install'"
    );
    let sd_active = sd_watchdog::is_active();
    check!(
        "Systemd notify socket",
        sd_active || !unit_installed,
        "NOTIFY_SOCKET not set — daemon may not be running under systemd"
    );
    if unit_installed {
        if let Ok(contents) = std::fs::read_to_string(crate::cli::service::UNIT_FILE_PATH) {
            let has_notify = contents.contains("Type=notify");
            check!(
                "Unit file has Type=notify",
                has_notify,
                "reinstall with 'syfrah fabric service install'"
            );
            let has_watchdog = contents.contains("WatchdogSec=");
            check!(
                "Unit file has WatchdogSec",
                has_watchdog,
                "reinstall with 'syfrah fabric service install'"
            );
        }
    }

    // -- Zone-specific diagnosis --
    let zone_diagnosis = if let Some(ref zone_name) = zone_filter {
        let target_zone = Zone::new(zone_name).ok_or_else(|| {
            anyhow::anyhow!("Invalid zone name '{zone_name}'. Use lowercase alphanumeric characters and hyphens.")
        })?;

        if !json {
            println!();
            ui::heading(&format!("Zone: {zone_name}"));
        }

        if let Ok(ref s) = state {
            let view = TopologyView::from_peers(&s.peers);
            let zone_peers = view.peers_in_zone(&target_zone);

            if zone_peers.is_empty() {
                if !json {
                    ui::warn(&format!("No nodes found in zone '{zone_name}'."));
                }
                check!(
                    format!("Zone '{zone_name}' has nodes"),
                    false,
                    "no nodes found in this zone"
                );
                None
            } else {
                let total_nodes = zone_peers.len();
                let active_nodes = zone_peers
                    .iter()
                    .filter(|p| p.status == PeerStatus::Active)
                    .count();

                // Build a WG public key -> WG peer summary map for handshake info
                let wg_stats: std::collections::HashMap<String, &wg::PeerSummary> =
                    if let Ok(ref summary) = wg_summary {
                        summary
                            .peers
                            .iter()
                            .map(|p| (p.public_key.clone(), p))
                            .collect()
                    } else {
                        std::collections::HashMap::new()
                    };

                let health = store::get_zone_health(zone_name)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| ZoneHealthStatus::from_counts(active_nodes, total_nodes));

                let node_word = if total_nodes == 1 { "node" } else { "nodes" };
                check!(
                    format!("Zone '{zone_name}': {total_nodes} {node_word}, {active_nodes} active"),
                    active_nodes > 0,
                    "no active nodes in zone"
                );

                if !json {
                    println!();
                    println!(
                        "Zone {}: {} {}, {} active",
                        zone_name, total_nodes, node_word, active_nodes
                    );
                    println!();
                }

                let mut node_statuses = Vec::new();
                for peer in zone_peers {
                    let status_str = match peer.status {
                        PeerStatus::Active => "ACTIVE",
                        PeerStatus::Unreachable => "UNREACHABLE",
                        PeerStatus::Removed => "REMOVED",
                    };

                    let handshake_ago = wg_stats
                        .get(&peer.wg_public_key)
                        .and_then(|wp| wp.last_handshake)
                        .and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .map(|d| fmt_ago(d.as_secs()))
                        });

                    if !json {
                        let handshake_label = handshake_ago
                            .as_deref()
                            .map(|a| format!(" (last handshake {a})"))
                            .unwrap_or_default();
                        println!("  {}: {}{}", peer.name, status_str, handshake_label);
                    }

                    node_statuses.push(ZoneNodeStatus {
                        name: peer.name.clone(),
                        status: status_str.to_string(),
                        last_handshake_ago: handshake_ago,
                    });
                }

                // Determine possible causes and next steps based on health
                let (possible_causes, next_steps) =
                    diagnose_zone_causes(active_nodes, total_nodes, &health);

                if !json && !possible_causes.is_empty() {
                    println!();
                    println!("Possible causes:");
                    for cause in &possible_causes {
                        println!("  - {cause}");
                    }
                }

                if !json && !next_steps.is_empty() {
                    println!();
                    println!("Next steps:");
                    for (i, step) in next_steps.iter().enumerate() {
                        println!("  {}. {step}", i + 1);
                    }
                }

                Some(ZoneDiagnosis {
                    zone: zone_name.clone(),
                    total_nodes,
                    active_nodes,
                    health: health.to_string(),
                    nodes: node_statuses,
                    possible_causes,
                    next_steps,
                })
            }
        } else {
            if !json {
                ui::warn("Cannot diagnose zone: mesh state not loaded.");
            }
            None
        }
    } else {
        None
    };

    if json {
        let total = pass_count + fail_count;
        let output = DiagnoseOutput {
            checks,
            passed: pass_count,
            failed: fail_count,
            total,
            zone_diagnosis,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!();

        // -- Summary --
        let total = pass_count + fail_count;
        if fail_count == 0 {
            ui::success(&format!(
                "{pass_count}/{total} checks passed. Fabric is healthy."
            ));
        } else {
            ui::warn(&format!("{fail_count}/{total} checks failed."));
        }
    }

    Ok(())
}

/// Determine possible causes and remediation steps for a zone based on its health.
fn diagnose_zone_causes(
    active: usize,
    total: usize,
    health: &ZoneHealthStatus,
) -> (Vec<String>, Vec<String>) {
    if total == 0 {
        return (vec![], vec![]);
    }

    match health {
        ZoneHealthStatus::Healthy => {
            // All good — no causes or steps needed
            (vec![], vec![])
        }
        ZoneHealthStatus::Degraded => {
            let unreachable = total - active;
            (
                vec![
                    format!("{unreachable} of {total} nodes unreachable (partial degradation)"),
                    "Network instability or rolling restart in progress".to_string(),
                    "Individual node daemon crash".to_string(),
                ],
                vec![
                    "SSH into unreachable nodes and check daemon status".to_string(),
                    "Check network connectivity between nodes".to_string(),
                    "Review logs: ~/.syfrah/syfrah.log on affected nodes".to_string(),
                ],
            )
        }
        ZoneHealthStatus::Critical => {
            let unreachable = total - active;
            (
                vec![
                    format!("{unreachable} of {total} nodes unreachable (critical)"),
                    "Possible network partition affecting most of the zone".to_string(),
                    "Provider-level issue (partial datacenter outage)".to_string(),
                ],
                vec![
                    "SSH into a node in the zone and check daemon status".to_string(),
                    "Check provider status page".to_string(),
                    "Review logs: ~/.syfrah/syfrah.log on affected nodes".to_string(),
                ],
            )
        }
        ZoneHealthStatus::Failed => (
            vec![
                "Entire zone offline (datacenter power/network outage)".to_string(),
                "Network partition from this node to the zone".to_string(),
                format!("Daemon crashed on all {total} nodes"),
            ],
            vec![
                "SSH into a node in the zone and check daemon status".to_string(),
                "Check provider status page".to_string(),
                format!("If offline > 30m: syfrah fabric zone drain <zone-name>"),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_ago_epoch_zero() {
        let result = fmt_ago(0);
        // epoch 0 is decades ago, must contain "d ago"
        assert!(result.contains("d ago"), "unexpected: {result}");
    }

    #[test]
    fn fmt_ago_future_returns_just_now() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 1000;
        assert_eq!(fmt_ago(future), "just now");
    }

    #[test]
    fn diagnose_healthy_zone_no_causes() {
        let (causes, steps) = diagnose_zone_causes(3, 3, &ZoneHealthStatus::Healthy);
        assert!(causes.is_empty());
        assert!(steps.is_empty());
    }

    #[test]
    fn diagnose_failed_zone_has_causes_and_steps() {
        let (causes, steps) = diagnose_zone_causes(0, 2, &ZoneHealthStatus::Failed);
        assert!(!causes.is_empty());
        assert!(!steps.is_empty());
        assert!(causes.iter().any(|c| c.contains("Entire zone offline")));
        assert!(steps.iter().any(|s| s.contains("SSH into")));
    }

    #[test]
    fn diagnose_degraded_zone_mentions_unreachable() {
        let (causes, _steps) = diagnose_zone_causes(1, 2, &ZoneHealthStatus::Degraded);
        assert!(causes.iter().any(|c| c.contains("1 of 2")));
    }

    #[test]
    fn diagnose_critical_zone_has_steps() {
        let (causes, steps) = diagnose_zone_causes(1, 4, &ZoneHealthStatus::Critical);
        assert!(!causes.is_empty());
        assert!(causes.iter().any(|c| c.contains("3 of 4")));
        assert!(steps.iter().any(|s| s.contains("provider status")));
    }

    #[test]
    fn diagnose_empty_zone_no_causes() {
        let (causes, steps) = diagnose_zone_causes(0, 0, &ZoneHealthStatus::Healthy);
        assert!(causes.is_empty());
        assert!(steps.is_empty());
    }

    #[test]
    fn diagnose_output_json_serializes_all_fields() {
        let output = DiagnoseOutput {
            checks: vec![
                DiagnoseCheck {
                    name: "Mesh state exists".to_string(),
                    passed: true,
                    detail: None,
                },
                DiagnoseCheck {
                    name: "Daemon process".to_string(),
                    passed: false,
                    detail: Some("daemon is not running".to_string()),
                },
            ],
            passed: 1,
            failed: 1,
            total: 2,
            zone_diagnosis: None,
        };

        let json_str = serde_json::to_string(&output).expect("serialization failed");
        let val: serde_json::Value = serde_json::from_str(&json_str).expect("invalid JSON");

        assert_eq!(val["passed"], 1);
        assert_eq!(val["failed"], 1);
        assert_eq!(val["total"], 2);
        assert_eq!(val["checks"].as_array().unwrap().len(), 2);
        assert_eq!(val["checks"][0]["name"], "Mesh state exists");
        assert_eq!(val["checks"][0]["passed"], true);
        assert!(val["checks"][0].get("detail").is_none());
        assert_eq!(val["checks"][1]["passed"], false);
        assert_eq!(val["checks"][1]["detail"], "daemon is not running");
        assert!(val.get("zone_diagnosis").is_none());
    }

    #[test]
    fn diagnose_output_json_with_zone_diagnosis() {
        let output = DiagnoseOutput {
            checks: vec![],
            passed: 0,
            failed: 0,
            total: 0,
            zone_diagnosis: Some(ZoneDiagnosis {
                zone: "par-1".to_string(),
                total_nodes: 3,
                active_nodes: 2,
                health: "Degraded".to_string(),
                nodes: vec![
                    ZoneNodeStatus {
                        name: "node-1".to_string(),
                        status: "ACTIVE".to_string(),
                        last_handshake_ago: Some("5s ago".to_string()),
                    },
                    ZoneNodeStatus {
                        name: "node-2".to_string(),
                        status: "UNREACHABLE".to_string(),
                        last_handshake_ago: None,
                    },
                ],
                possible_causes: vec!["1 of 3 nodes unreachable".to_string()],
                next_steps: vec!["SSH into unreachable nodes".to_string()],
            }),
        };

        let json_str = serde_json::to_string(&output).expect("serialization failed");
        let val: serde_json::Value = serde_json::from_str(&json_str).expect("invalid JSON");

        let zd = &val["zone_diagnosis"];
        assert_eq!(zd["zone"], "par-1");
        assert_eq!(zd["total_nodes"], 3);
        assert_eq!(zd["active_nodes"], 2);
        assert_eq!(zd["health"], "Degraded");
        assert_eq!(zd["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(zd["nodes"][0]["name"], "node-1");
        assert_eq!(zd["nodes"][1]["status"], "UNREACHABLE");
        assert!(zd["nodes"][1].get("last_handshake_ago").is_none());
        assert_eq!(zd["possible_causes"].as_array().unwrap().len(), 1);
        assert_eq!(zd["next_steps"].as_array().unwrap().len(), 1);
    }
}
