use crate::sanitize::sanitize;
use crate::{config, no_mesh_error, store, ui, wg};
use anyhow::Result;
use serde::Serialize;

/// Options for the status command.
pub struct StatusOpts {
    /// Show config and metrics sections.
    pub verbose: bool,
    /// Show the full mesh secret instead of masking it.
    pub show_secret: bool,
    /// Output as JSON.
    pub json: bool,
}

pub async fn run(opts: StatusOpts) -> Result<()> {
    let tuning = config::load_tuning().unwrap_or_default();
    wg::set_interface_name(&tuning.interface_name);

    let state = store::load().map_err(|_| no_mesh_error())?;

    if opts.json {
        return run_json(&state, &opts);
    }

    // ── Uptime (computed early for the Mesh box) ────────────────────
    let m = &state.metrics;
    let uptime_str = if m.daemon_started_at > 0 {
        let uptime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(m.daemon_started_at);
        Some(fmt_duration(uptime))
    } else {
        None
    };

    // ── Mesh section ────────────────────────────────────────────────
    let region = state
        .region
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| "(not set)".into());
    let zone = state
        .zone
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| "(not set)".into());

    ui::box_top("Mesh");
    ui::box_row(&format!("Name:     {}", sanitize(&state.mesh_name)));
    ui::box_row(&format!("Node:     {}", sanitize(&state.node_name)));
    ui::box_row(&format!("Region:   {}", region));
    ui::box_row(&format!("Zone:     {}", zone));
    ui::box_row(&format!("Prefix:   {}/48", state.mesh_prefix));
    if let Some(ref up) = uptime_str {
        ui::box_row(&format!("Uptime:   {up}"));
    }
    ui::box_bottom();

    // ── Health status (prominent, outside a box) ────────────────────
    println!();
    let daemon_running = store::daemon_running();
    match daemon_running {
        Some(pid) => ui::health_ok(&format!("Daemon running (pid {pid})")),
        None => ui::health_bad("Daemon stopped"),
    }

    let iface_up = match wg::interface_summary() {
        Ok(summary) => summary.public_key.is_some(),
        Err(_) => false,
    };
    if iface_up {
        ui::health_ok(&format!("Interface {} is up", wg::interface_name()));
    } else {
        ui::health_bad(&format!("Interface {} is down", wg::interface_name()));
    }

    // Gateway role status
    let gw = config::load_gateway_config();
    if gw.enabled {
        if daemon_running.is_some() {
            ui::health_ok(&format!(
                "Gateway: active (port {})",
                gw.bind_address.port()
            ));
        } else {
            ui::health_bad(&format!(
                "Gateway: configured (port {}) but daemon stopped",
                gw.bind_address.port()
            ));
        }
    } else {
        ui::info_line("Gateway", "disabled");
    }
    println!();

    // TODO(#644): Add compute summary (runtime type, VM count, running VMs)
    // to fabric status output. This requires cross-layer integration:
    // the fabric daemon would need to query the compute layer's control
    // socket (ComputeRequest::Status) and merge the response into the
    // fabric status view. Skipped for now because fabric cannot depend
    // on compute (lower layers never depend on higher layers). A shared
    // status aggregation mechanism is needed first.

    // ── Peers section ───────────────────────────────────────────────
    let total = state.peers.len();
    let active = state
        .peers
        .iter()
        .filter(|p| p.status == syfrah_core::mesh::PeerStatus::Active)
        .count();
    let unreachable = state
        .peers
        .iter()
        .filter(|p| p.status == syfrah_core::mesh::PeerStatus::Unreachable)
        .count();

    ui::box_top(&format!("Peers ({total})"));
    if ui::is_tty() {
        let green = console::Style::new().green();
        let red = console::Style::new().red();
        ui::box_row(&format!("{} {} active", green.apply_to("\u{25cf}"), active));
        if unreachable > 0 {
            ui::box_row(&format!(
                "{} {} unreachable",
                red.apply_to("\u{2717}"),
                unreachable
            ));
        } else {
            ui::box_row(&format!(
                "{} {} unreachable",
                green.apply_to("\u{25cf}"),
                unreachable
            ));
        }
    } else {
        ui::box_row(&format!("{active} active"));
        ui::box_row(&format!("{unreachable} unreachable"));
    }
    ui::box_bottom();
    println!();

    // ── Network section ─────────────────────────────────────────────
    let secret_display = if opts.show_secret {
        state.mesh_secret.clone()
    } else {
        format!(
            "{} (use --show-secret)",
            ui::mask_secret(&state.mesh_secret)
        )
    };

    ui::box_top("Network");
    ui::box_row(&format!("WireGuard:  port {}", state.wg_listen_port));
    if daemon_running.is_some() {
        ui::box_row(&format!("Peering:    active (port {})", state.peering_port));
    } else {
        ui::box_row(&format!(
            "Peering:    inactive (port {} configured)",
            state.peering_port
        ));
    }
    ui::box_row(&format!("Mesh IPv6:  {}", state.mesh_ipv6));
    ui::box_row(&format!("Secret:     {secret_display}"));

    // WG traffic summary if interface is up
    if let Ok(summary) = wg::interface_summary() {
        let (rx, tx) = summary.peers.iter().fold((0u64, 0u64), |(rx, tx), p| {
            (rx + p.rx_bytes, tx + p.tx_bytes)
        });
        ui::box_row(&format!(
            "Traffic:    rx {} / tx {}",
            fmt_bytes(rx),
            fmt_bytes(tx)
        ));
    }
    ui::box_bottom();

    // ── Verbose: Zone Health ────────────────────────────────────────
    if opts.verbose {
        if let Ok(zone_statuses) = store::list_zone_health() {
            if !zone_statuses.is_empty() {
                ui::box_top("Zone Health");
                let mut sorted = zone_statuses;
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                for (zone_name, status) in &sorted {
                    let label = status.to_string();
                    if ui::is_tty() {
                        let styled = match status {
                            crate::events::ZoneHealthStatus::Healthy => {
                                let s = console::Style::new().green();
                                format!("{}", s.apply_to(&label))
                            }
                            crate::events::ZoneHealthStatus::Degraded => {
                                let s = console::Style::new().yellow();
                                format!("{}", s.apply_to(&label))
                            }
                            _ => {
                                let s = console::Style::new().red().bold();
                                format!("{}", s.apply_to(&label))
                            }
                        };
                        ui::box_row(&format!("{zone_name}: {styled}"));
                    } else {
                        ui::box_row(&format!("{zone_name}: {label}"));
                    }
                }
                ui::box_bottom();
                println!();
            }
        }
    }

    // ── Verbose: Metrics ────────────────────────────────────────────
    if opts.verbose && m.daemon_started_at > 0 {
        println!();
        ui::box_top("Metrics");
        ui::box_row(&format!("Peers discovered:  {}", m.peers_discovered));
        ui::box_row(&format!("WG reconciles:     {}", m.wg_reconciliations));
        ui::box_row(&format!(
            "Peers unreached:   {}",
            m.peers_marked_unreachable
        ));
        ui::box_row(&format!("Announce fails:    {}", m.announcements_failed));
        ui::box_row(&format!("Announces queued:  {}", m.announces_queued));
        ui::box_row(&format!("Queue overflows:   {}", m.announces_queue_full));
        ui::box_bottom();
    }

    // ── Verbose: Config ─────────────────────────────────────────────
    if opts.verbose {
        let tuning = config::load_tuning().unwrap_or_default();
        println!();
        ui::box_top("Config");
        ui::box_row(&format!(
            "health_check_interval:  {}s",
            tuning.health_check_interval.as_secs()
        ));
        ui::box_row(&format!(
            "reconcile_interval:     {}s",
            tuning.reconcile_interval.as_secs()
        ));
        ui::box_row(&format!(
            "persist_interval:       {}s",
            tuning.persist_interval.as_secs()
        ));
        ui::box_row(&format!(
            "unreachable_timeout:    {}s",
            tuning.unreachable_timeout.as_secs()
        ));
        ui::box_bottom();
    }

    Ok(())
}

fn fmt_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

fn run_json(state: &store::NodeState, opts: &StatusOpts) -> Result<()> {
    let daemon_pid = store::daemon_running();
    let iface_up = wg::interface_summary()
        .map(|s| s.public_key.is_some())
        .unwrap_or(false);

    let total_peers = state.peers.len();
    let active = state
        .peers
        .iter()
        .filter(|p| p.status == syfrah_core::mesh::PeerStatus::Active)
        .count();
    let unreachable = state
        .peers
        .iter()
        .filter(|p| p.status == syfrah_core::mesh::PeerStatus::Unreachable)
        .count();

    let (rx, tx) = wg::interface_summary()
        .map(|s| {
            s.peers.iter().fold((0u64, 0u64), |(rx, tx), p| {
                (rx + p.rx_bytes, tx + p.tx_bytes)
            })
        })
        .unwrap_or((0, 0));

    let secret_display = if opts.show_secret {
        state.mesh_secret.clone()
    } else {
        ui::mask_secret(&state.mesh_secret)
    };

    let tuning = config::load_tuning().unwrap_or_default();

    let gw = config::load_gateway_config();

    let output = StatusJson {
        mesh_name: &state.mesh_name,
        node_name: &state.node_name,
        region: state.region.as_deref(),
        zone: state.zone.as_deref(),
        mesh_prefix: state.mesh_prefix.to_string(),
        mesh_ipv6: state.mesh_ipv6.to_string(),
        wg_listen_port: state.wg_listen_port,
        peering_port: state.peering_port,
        daemon_pid,
        interface_up: iface_up,
        peers_total: total_peers,
        peers_active: active,
        peers_unreachable: unreachable,
        secret: secret_display,
        traffic_rx: rx,
        traffic_tx: tx,
        gateway: JsonGateway {
            enabled: gw.enabled,
            port: if gw.enabled {
                Some(gw.bind_address.port())
            } else {
                None
            },
        },
        metrics: JsonMetrics {
            daemon_started_at: state.metrics.daemon_started_at,
            peers_discovered: state.metrics.peers_discovered,
            wg_reconciliations: state.metrics.wg_reconciliations,
            peers_marked_unreachable: state.metrics.peers_marked_unreachable,
            announcements_failed: state.metrics.announcements_failed,
            announces_queued: state.metrics.announces_queued,
            announces_queue_full: state.metrics.announces_queue_full,
        },
        config: JsonConfig {
            health_check_interval_secs: tuning.health_check_interval.as_secs(),
            reconcile_interval_secs: tuning.reconcile_interval.as_secs(),
            persist_interval_secs: tuning.persist_interval.as_secs(),
            unreachable_timeout_secs: tuning.unreachable_timeout.as_secs(),
        },
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[derive(Serialize)]
struct StatusJson<'a> {
    mesh_name: &'a str,
    node_name: &'a str,
    region: Option<&'a str>,
    zone: Option<&'a str>,
    mesh_prefix: String,
    mesh_ipv6: String,
    wg_listen_port: u16,
    peering_port: u16,
    daemon_pid: Option<u32>,
    interface_up: bool,
    peers_total: usize,
    peers_active: usize,
    peers_unreachable: usize,
    secret: String,
    traffic_rx: u64,
    traffic_tx: u64,
    gateway: JsonGateway,
    metrics: JsonMetrics,
    config: JsonConfig,
}

#[derive(Serialize)]
struct JsonGateway {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

#[derive(Serialize)]
struct JsonMetrics {
    daemon_started_at: u64,
    peers_discovered: u64,
    wg_reconciliations: u64,
    peers_marked_unreachable: u64,
    announcements_failed: u64,
    announces_queued: u64,
    announces_queue_full: u64,
}

#[derive(Serialize)]
struct JsonConfig {
    health_check_interval_secs: u64,
    reconcile_interval_secs: u64,
    persist_interval_secs: u64,
    unreachable_timeout_secs: u64,
}

fn fmt_bytes(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else if b < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_duration_cases() {
        assert_eq!(fmt_duration(30), "30s");
        assert_eq!(fmt_duration(90), "1m 30s");
        assert_eq!(fmt_duration(3661), "1h 1m");
        assert_eq!(fmt_duration(90061), "1d 1h");
    }

    #[test]
    fn fmt_bytes_cases() {
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(2048), "2.0 KiB");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
