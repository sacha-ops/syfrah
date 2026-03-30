use std::collections::HashMap;
use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use syfrah_core::mesh::{PeerStatus, Region, Zone};

use crate::events::ZoneHealthStatus;
use crate::topology::TopologyView;
use crate::{no_mesh_error, store, ui};

// ── drain ───────────────────────────────────────────────────────────────

pub async fn drain(zone_path: &str, yes: bool) -> Result<()> {
    let (region_str, zone_str) = parse_zone_path(zone_path)?;
    let state = store::load().map_err(|_| no_mesh_error())?;
    let view = TopologyView::from_peers(&state.peers);

    // Validate region/zone exist in the mesh
    let region = Region::new(&region_str)
        .ok_or_else(|| anyhow::anyhow!("Invalid region name '{region_str}'"))?;
    let zone =
        Zone::new(&zone_str).ok_or_else(|| anyhow::anyhow!("Invalid zone name '{zone_str}'"))?;

    validate_zone_exists(&view, &region, &zone, zone_path)?;

    // Check if already draining
    if let Ok(Some(true)) = store::get_zone_drain(zone.as_str()) {
        println!("Zone {zone_path} is already draining.");
        return Ok(());
    }

    // Confirmation unless --yes
    if !yes {
        let active = view.active_count_in_zone(&zone);
        let node_word = if active == 1 { "node" } else { "nodes" };
        print!(
            "Drain zone {zone_path}? This will stop new workload placement ({active} active {node_word}). [y/N] "
        );
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    store::set_zone_drain(zone.as_str(), true)?;

    let active = view.active_count_in_zone(&zone);
    let node_word = if active == 1 { "node" } else { "nodes" };
    println!("OK: Zone {zone_path} marked as draining. {active} active {node_word}.");
    Ok(())
}

// ── undrain ─────────────────────────────────────────────────────────────

pub async fn undrain(zone_path: &str) -> Result<()> {
    let (region_str, zone_str) = parse_zone_path(zone_path)?;
    let state = store::load().map_err(|_| no_mesh_error())?;
    let view = TopologyView::from_peers(&state.peers);

    let region = Region::new(&region_str)
        .ok_or_else(|| anyhow::anyhow!("Invalid region name '{region_str}'"))?;
    let zone =
        Zone::new(&zone_str).ok_or_else(|| anyhow::anyhow!("Invalid zone name '{zone_str}'"))?;

    validate_zone_exists(&view, &region, &zone, zone_path)?;

    // Check if not draining
    let is_draining = store::get_zone_drain(zone.as_str())?.unwrap_or(false);
    if !is_draining {
        println!("Zone {zone_path} is not draining.");
        return Ok(());
    }

    store::set_zone_drain(zone.as_str(), false)?;

    println!("OK: Zone {zone_path} restored to active.");
    Ok(())
}

// ── status ──────────────────────────────────────────────────────────────

pub async fn status(json: bool) -> Result<()> {
    let state = store::load().map_err(|_| no_mesh_error())?;
    let view = TopologyView::from_peers(&state.peers);

    // Collect drain state for all zones
    let drain_map: HashMap<String, bool> = store::list_zone_drain()
        .unwrap_or_default()
        .into_iter()
        .collect();

    if json {
        return status_json(&view, &drain_map);
    }

    status_table(&view, &drain_map)
}

fn status_table(view: &TopologyView, drain_map: &HashMap<String, bool>) -> Result<()> {
    let mut regions: Vec<&Region> = view.regions();
    regions.sort_by_key(|r| r.as_str().to_owned());

    // Header
    println!(
        "{:<14} {:<14} {:>5}  {:>6}  STATUS",
        "REGION", "ZONE", "NODES", "ACTIVE"
    );

    for region in &regions {
        let mut zones = view.zones_in_region(region);
        zones.sort_by_key(|z| z.as_str().to_owned());

        for zone in &zones {
            let peers = view.peers_in_zone(zone);
            let total = peers.len();
            let active = peers
                .iter()
                .filter(|p| p.status == PeerStatus::Active)
                .count();

            let is_draining = drain_map.get(zone.as_str()).copied().unwrap_or(false);

            let status_str = if is_draining {
                zone_status_label("DRAINING")
            } else {
                // Determine health-based status
                let health = store::get_zone_health(zone.as_str()).ok().flatten();
                match health {
                    Some(ZoneHealthStatus::Healthy) => zone_status_label("ACTIVE"),
                    Some(ZoneHealthStatus::Degraded) => zone_status_label("DEGRADED"),
                    Some(ZoneHealthStatus::Critical) => zone_status_label("CRITICAL"),
                    Some(ZoneHealthStatus::Failed) => zone_status_label("FAILED"),
                    None => {
                        // No health data yet — derive from peer counts
                        if total == 0 {
                            zone_status_label("EMPTY")
                        } else if active == total {
                            zone_status_label("ACTIVE")
                        } else if active == 0 {
                            zone_status_label("FAILED")
                        } else {
                            zone_status_label("DEGRADED")
                        }
                    }
                }
            };

            println!(
                "{:<14} {:<14} {:>5}  {:>6}  {}",
                region.as_str(),
                zone.as_str(),
                total,
                active,
                status_str,
            );
        }
    }

    Ok(())
}

fn status_json(view: &TopologyView, drain_map: &HashMap<String, bool>) -> Result<()> {
    let mut regions: Vec<&Region> = view.regions();
    regions.sort_by_key(|r| r.as_str().to_owned());

    let mut zone_entries = Vec::new();

    for region in &regions {
        let mut zones = view.zones_in_region(region);
        zones.sort_by_key(|z| z.as_str().to_owned());

        for zone in &zones {
            let peers = view.peers_in_zone(zone);
            let total = peers.len();
            let active = peers
                .iter()
                .filter(|p| p.status == PeerStatus::Active)
                .count();

            let is_draining = drain_map.get(zone.as_str()).copied().unwrap_or(false);

            let status_str = if is_draining {
                "DRAINING".to_string()
            } else {
                let health = store::get_zone_health(zone.as_str()).ok().flatten();
                match health {
                    Some(ZoneHealthStatus::Healthy) => "ACTIVE".to_string(),
                    Some(ZoneHealthStatus::Degraded) => "DEGRADED".to_string(),
                    Some(ZoneHealthStatus::Critical) => "CRITICAL".to_string(),
                    Some(ZoneHealthStatus::Failed) => "FAILED".to_string(),
                    None => {
                        if total == 0 {
                            "EMPTY".to_string()
                        } else if active == total {
                            "ACTIVE".to_string()
                        } else if active == 0 {
                            "FAILED".to_string()
                        } else {
                            "DEGRADED".to_string()
                        }
                    }
                }
            };

            zone_entries.push(JsonZoneStatus {
                region: region.as_str().to_owned(),
                zone: zone.as_str().to_owned(),
                nodes: total,
                active,
                status: status_str,
                draining: is_draining,
            });
        }
    }

    let output = JsonZoneStatusList {
        zones: zone_entries,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────

/// Parse "region/zone" path into (region, zone) components.
fn parse_zone_path(path: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = path.splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "Invalid zone path '{path}'. Expected format: region/zone (e.g. eu-west/par-ovh)"
        );
    }
    Ok((parts[0].to_owned(), parts[1].to_owned()))
}

/// Validate that the zone exists in the current topology.
fn validate_zone_exists(
    view: &TopologyView,
    region: &Region,
    zone: &Zone,
    zone_path: &str,
) -> Result<()> {
    // Check if the region has any peers
    if view.peers_in_region(region).is_empty() {
        let available: Vec<String> = view
            .regions()
            .iter()
            .map(|r| r.as_str().to_owned())
            .collect();
        anyhow::bail!(
            "Region '{}' not found. Available regions: {}",
            region.as_str(),
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        );
    }

    // Check if the zone has any peers
    if view.peers_in_zone(zone).is_empty() {
        let available: Vec<String> = view
            .zones_in_region(region)
            .iter()
            .map(|z| z.as_str().to_owned())
            .collect();
        anyhow::bail!(
            "Zone '{zone_path}' not found. Available zones in '{}': {}",
            region.as_str(),
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        );
    }

    Ok(())
}

/// Format a zone status label with color when on a TTY.
fn zone_status_label(label: &str) -> String {
    if !ui::use_color() {
        return label.to_string();
    }
    match label {
        "ACTIVE" => {
            let style = console::Style::new().green();
            format!("{}", style.apply_to(label))
        }
        "DRAINING" => {
            let style = console::Style::new().yellow().bold();
            format!("{}", style.apply_to(label))
        }
        "DEGRADED" => {
            let style = console::Style::new().yellow();
            format!("{}", style.apply_to(label))
        }
        "CRITICAL" | "FAILED" => {
            let style = console::Style::new().red().bold();
            format!("{}", style.apply_to(label))
        }
        _ => label.to_string(),
    }
}

// ── JSON types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonZoneStatusList {
    zones: Vec<JsonZoneStatus>,
}

#[derive(Serialize)]
struct JsonZoneStatus {
    region: String,
    zone: String,
    nodes: usize,
    active: usize,
    status: String,
    draining: bool,
}

// ── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_zone_path_valid() {
        let (region, zone) = parse_zone_path("eu-west/par-ovh").unwrap();
        assert_eq!(region, "eu-west");
        assert_eq!(zone, "par-ovh");
    }

    #[test]
    fn parse_zone_path_missing_slash() {
        let err = parse_zone_path("eu-west-par-ovh").unwrap_err();
        assert!(err.to_string().contains("Expected format"));
    }

    #[test]
    fn parse_zone_path_empty_region() {
        let err = parse_zone_path("/par-ovh").unwrap_err();
        assert!(err.to_string().contains("Expected format"));
    }

    #[test]
    fn parse_zone_path_empty_zone() {
        let err = parse_zone_path("eu-west/").unwrap_err();
        assert!(err.to_string().contains("Expected format"));
    }

    #[test]
    fn zone_status_label_plain() {
        // In test (non-TTY) should return plain string
        assert_eq!(zone_status_label("ACTIVE"), "ACTIVE");
        assert_eq!(zone_status_label("DRAINING"), "DRAINING");
        assert_eq!(zone_status_label("DEGRADED"), "DEGRADED");
    }
}
