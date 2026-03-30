use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use syfrah_core::mesh::{PeerStatus, Region, Zone};

use crate::cli::ui::truncate;
use crate::config;
use crate::events::ZoneHealthStatus;
use crate::topology::TopologyView;
use crate::{no_mesh_error, store, ui, wg};

/// Options for the topology command.
pub struct TopologyOpts {
    /// Filter to a single region.
    pub region: Option<String>,
    /// Filter to a single zone.
    pub zone: Option<String>,
    /// Output as JSON.
    pub json: bool,
    /// Show per-node endpoint, handshake, and traffic.
    pub verbose: bool,
}

pub async fn run(opts: TopologyOpts) -> Result<()> {
    use syfrah_core::mesh::{PeerRecord, Topology};

    let state = store::load().map_err(|_| no_mesh_error())?;

    // Include the local node in the topology view alongside remote peers.
    let local_peer = PeerRecord {
        name: state.node_name.clone(),
        wg_public_key: state.wg_public_key.clone(),
        endpoint: state
            .public_endpoint
            .unwrap_or_else(|| format!("0.0.0.0:{}", state.wg_listen_port).parse().unwrap()),
        mesh_ipv6: state.mesh_ipv6,
        last_seen: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        status: PeerStatus::Active,
        region: state.region.clone(),
        zone: state.zone.clone(),
        topology: Topology::from_strings(state.region.as_deref(), state.zone.as_deref()),
    };
    let mut all_nodes = state.peers.clone();
    all_nodes.push(local_peer);

    let view = TopologyView::from_peers(&all_nodes);

    if opts.json {
        return run_json(&state.mesh_name, &view, &opts);
    }

    run_tree(&state.mesh_name, &view, &opts)
}

fn run_tree(mesh_name: &str, view: &TopologyView, opts: &TopologyOpts) -> Result<()> {
    let gw = config::load_gateway_config();
    let local_node_name = store::load().map(|s| s.node_name).unwrap_or_default();

    // Resolve region filter
    let mut regions: Vec<&Region> = view.regions();
    regions.sort_by_key(|r| r.as_str().to_owned());

    if let Some(ref filter) = opts.region {
        let target = Region::new(filter);
        match target {
            Some(ref r) if regions.contains(&r) => {
                regions.retain(|rr| *rr == r);
            }
            _ => {
                let available: Vec<&str> = regions.iter().map(|r| r.as_str()).collect();
                anyhow::bail!(
                    "No region '{}'. Available: {}.",
                    filter,
                    available.join(", ")
                );
            }
        }
    }

    // Zone filter validation (applied during rendering)
    if let Some(ref filter) = opts.zone {
        let target = Zone::new(filter);
        let all_zones: Vec<&Zone> = regions
            .iter()
            .flat_map(|r| view.zones_in_region(r))
            .collect();
        match target {
            Some(ref z) if all_zones.contains(&z) => {}
            _ => {
                let available: Vec<&str> = all_zones.iter().map(|z| z.as_str()).collect();
                anyhow::bail!("No zone '{}'. Available: {}.", filter, available.join(", "));
            }
        }
    }

    // Count totals for the header
    let total_nodes: usize = regions.iter().map(|r| view.peers_in_region(r).len()).sum();
    let total_zones: usize = regions.iter().map(|r| view.zones_in_region(r).len()).sum();

    // Header box
    ui::box_top("Topology");
    ui::box_row(&format!(
        "Mesh: {}  |  Nodes: {}  |  Regions: {}  |  Zones: {}",
        mesh_name,
        total_nodes,
        regions.len(),
        total_zones,
    ));
    ui::box_bottom();
    println!();

    // Live WG stats for verbose mode
    let wg_stats = if opts.verbose {
        wg::interface_summary()
            .ok()
            .map(|s| {
                s.peers
                    .into_iter()
                    .map(|p| (p.public_key.clone(), p))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    for region in &regions {
        let region_count = view.peers_in_region(region).len();
        let node_word = if region_count == 1 { "node" } else { "nodes" };
        println!("{} ({} {})", region.as_str(), region_count, node_word);

        let mut zones = view.zones_in_region(region);
        zones.sort_by_key(|z| z.as_str().to_owned());

        for zone in &zones {
            // Apply zone filter
            if let Some(ref filter) = opts.zone {
                if let Some(ref target) = Zone::new(filter) {
                    if *zone != target {
                        continue;
                    }
                }
            }

            let peers = view.peers_in_zone(zone);
            let zone_count = peers.len();
            let zone_word = if zone_count == 1 { "node" } else { "nodes" };
            let zone_health = store::get_zone_health(zone.as_str()).ok().flatten();
            let health_tag = match zone_health {
                Some(status) => format_zone_health(status),
                None => String::new(),
            };
            if health_tag.is_empty() {
                println!("  {} ({} {})", zone.as_str(), zone_count, zone_word);
            } else {
                println!(
                    "  {} ({} {}) [{}]",
                    zone.as_str(),
                    zone_count,
                    zone_word,
                    health_tag,
                );
            }

            for peer in peers {
                let name = truncate(&peer.name, 20);
                let ipv6 = if opts.verbose {
                    peer.mesh_ipv6.to_string()
                } else {
                    truncate_ipv6(&peer.mesh_ipv6.to_string())
                };
                let status = format_status(peer.status);
                let role_tag = if gw.enabled && peer.name == local_node_name {
                    " [gateway]"
                } else {
                    ""
                };

                if opts.verbose {
                    println!("    {:<20}  {:<39}  {}{}", name, ipv6, status, role_tag);
                    if let Some(wg_peer) = wg_stats.get(&peer.wg_public_key) {
                        let endpoint = wg_peer
                            .endpoint
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "(none)".to_string());
                        let handshake = wg_peer
                            .last_handshake
                            .and_then(|t| {
                                t.duration_since(std::time::UNIX_EPOCH)
                                    .ok()
                                    .map(|d| fmt_ago(d.as_secs()))
                            })
                            .unwrap_or_else(|| "(never)".to_string());
                        let traffic = format!(
                            "rx {} / tx {}",
                            fmt_bytes(wg_peer.rx_bytes),
                            fmt_bytes(wg_peer.tx_bytes)
                        );
                        println!(
                            "    {:<20}  endpoint: {}  handshake: {}  {}",
                            "", endpoint, handshake, traffic
                        );
                    }
                } else {
                    println!("    {:<20}  {:<16}  {}{}", name, ipv6, status, role_tag);
                }
            }
        }
        println!();
    }

    Ok(())
}

fn run_json(mesh_name: &str, view: &TopologyView, opts: &TopologyOpts) -> Result<()> {
    let gw = config::load_gateway_config();
    let local_node_name = store::load().map(|s| s.node_name).unwrap_or_default();

    let mut regions: Vec<&Region> = view.regions();
    regions.sort_by_key(|r| r.as_str().to_owned());

    // Apply region filter
    if let Some(ref filter) = opts.region {
        let target = Region::new(filter);
        match target {
            Some(ref r) if regions.contains(&r) => {
                regions.retain(|rr| *rr == r);
            }
            _ => {
                let available: Vec<&str> = regions.iter().map(|r| r.as_str()).collect();
                anyhow::bail!(
                    "No region '{}'. Available: {}.",
                    filter,
                    available.join(", ")
                );
            }
        }
    }

    let total_nodes: usize = regions.iter().map(|r| view.peers_in_region(r).len()).sum();

    let json_regions: Vec<JsonRegion> = regions
        .iter()
        .map(|region| {
            let mut zones = view.zones_in_region(region);
            zones.sort_by_key(|z| z.as_str().to_owned());

            // Apply zone filter
            if let Some(ref filter) = opts.zone {
                if let Some(ref target) = Zone::new(filter) {
                    zones.retain(|z| *z == target);
                }
            }

            let json_zones: Vec<JsonZone> = zones
                .iter()
                .map(|zone| {
                    let peers = view.peers_in_zone(zone);
                    let nodes: Vec<JsonNode> = peers
                        .iter()
                        .map(|p| {
                            let role = if gw.enabled && p.name == local_node_name {
                                Some("gateway".to_string())
                            } else {
                                None
                            };
                            JsonNode {
                                name: p.name.clone(),
                                mesh_ipv6: p.mesh_ipv6.to_string(),
                                status: format_status(p.status),
                                role,
                            }
                        })
                        .collect();
                    let health = store::get_zone_health(zone.as_str())
                        .ok()
                        .flatten()
                        .map(|s| s.to_string());
                    JsonZone {
                        name: zone.as_str().to_owned(),
                        health,
                        nodes,
                    }
                })
                .collect();

            JsonRegion {
                name: region.as_str().to_owned(),
                zones: json_zones,
            }
        })
        .collect();

    let output = JsonTopology {
        mesh_name: mesh_name.to_owned(),
        total_nodes,
        regions: json_regions,
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn format_zone_health(status: ZoneHealthStatus) -> String {
    let label = status.to_string();
    if !ui::use_color() {
        return label;
    }
    match status {
        ZoneHealthStatus::Healthy => {
            let style = console::Style::new().green();
            format!("{}", style.apply_to(label))
        }
        ZoneHealthStatus::Degraded => {
            let style = console::Style::new().yellow();
            format!("{}", style.apply_to(label))
        }
        ZoneHealthStatus::Critical => {
            let style = console::Style::new().red().bold();
            format!("{}", style.apply_to(label))
        }
        ZoneHealthStatus::Failed => {
            let style = console::Style::new().red().bold();
            format!("{}", style.apply_to(label))
        }
    }
}

fn format_status(status: PeerStatus) -> String {
    match status {
        PeerStatus::Active => {
            if ui::use_color() {
                let green = console::Style::new().green();
                format!("{}", green.apply_to("active"))
            } else {
                "active".to_string()
            }
        }
        PeerStatus::Unreachable => {
            if ui::use_color() {
                let red = console::Style::new().red();
                format!("{}", red.apply_to("unreachable"))
            } else {
                "unreachable".to_string()
            }
        }
        PeerStatus::Removed => "removed".to_string(),
    }
}

/// Truncate an IPv6 address for compact display (first 2 groups + last group).
fn truncate_ipv6(addr: &str) -> String {
    let parts: Vec<&str> = addr.split(':').collect();
    if parts.len() <= 3 {
        return addr.to_string();
    }
    format!("{}:{}::..:{}", parts[0], parts[1], parts[parts.len() - 1])
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

fn fmt_ago(epoch_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let ago = now.saturating_sub(epoch_secs);
    if ago < 60 {
        format!("{ago}s ago")
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else {
        format!("{}h ago", ago / 3600)
    }
}

// ── JSON output types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonTopology {
    mesh_name: String,
    total_nodes: usize,
    regions: Vec<JsonRegion>,
}

#[derive(Serialize)]
struct JsonRegion {
    name: String,
    zones: Vec<JsonZone>,
}

#[derive(Serialize)]
struct JsonZone {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<String>,
    nodes: Vec<JsonNode>,
}

#[derive(Serialize)]
struct JsonNode {
    name: String,
    mesh_ipv6: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ipv6_short() {
        assert_eq!(truncate_ipv6("::1"), "::1");
    }

    #[test]
    fn truncate_ipv6_full() {
        let result = truncate_ipv6("fd27:0:0:0:0:0:0:e101");
        assert!(result.contains("fd27"));
        assert!(result.contains("e101"));
    }

    #[test]
    fn fmt_bytes_cases() {
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(2048), "2.0 KiB");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn fmt_ago_recent() {
        // Just test the formatting logic with known epoch
        let result = fmt_ago(0);
        assert!(result.contains("h ago") || result.contains("m ago"));
    }

    #[test]
    fn format_status_values() {
        // In test (non-TTY), should return plain strings
        assert_eq!(format_status(PeerStatus::Active), "active");
        assert_eq!(format_status(PeerStatus::Unreachable), "unreachable");
        assert_eq!(format_status(PeerStatus::Removed), "removed");
    }
}
