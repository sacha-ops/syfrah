use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::sanitize::sanitize;
use crate::topology::TopologyView;
use crate::{no_mesh_error, store, ui, wg};
use anyhow::Result;
use serde::Serialize;
use syfrah_core::mesh::{PeerRecord, PeerStatus, Region, Zone};

/// Options for the `peers` command.
pub struct PeersOpts {
    pub json: bool,
    pub topology: bool,
    pub region: Option<String>,
    pub zone: Option<String>,
}

pub async fn run(opts: PeersOpts) -> Result<()> {
    let state = store::load().map_err(|_| no_mesh_error())?;

    if state.peers.is_empty() {
        if opts.json {
            println!("[]");
            return Ok(());
        }
        ui::info_line("Peers", "No peers discovered yet.");
        return Ok(());
    }

    // Dedup peers by WG public key, keeping the entry with the latest last_seen
    let peers = dedup_peers(&state.peers);

    // Apply region/zone filters
    let peers = filter_peers(peers, opts.region.as_deref(), opts.zone.as_deref());

    if opts.json {
        let output: Vec<PeerJson> = peers
            .iter()
            .map(|p| PeerJson {
                name: &p.name,
                wg_public_key: &p.wg_public_key,
                endpoint: p.endpoint.to_string(),
                mesh_ipv6: p.mesh_ipv6.to_string(),
                last_seen: p.last_seen,
                status: match p.status {
                    PeerStatus::Active => "active",
                    PeerStatus::Unreachable => "unreachable",
                    PeerStatus::Removed => "removed",
                },
                region: p.region.as_deref(),
                zone: p.zone.as_deref(),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if opts.topology {
        return print_topology(&peers);
    }

    // Default flat table output
    let wg_summary = wg::interface_summary().ok();

    ui::heading(&format!(
        "{:<18} {:<12} {:<14} {:<40} {:<24} {:>8} {:<14} {:>12} {:>14}",
        "NAME",
        "REGION",
        "ZONE",
        "MESH_IPV6",
        "ENDPOINT",
        "STATUS",
        "SINCE",
        "HANDSHAKE",
        "TRAFFIC"
    ));

    for peer in &peers {
        print_peer_row(peer, &wg_summary);
    }

    Ok(())
}

/// Print topology-grouped output (tree view by region/zone).
fn print_topology(peers: &[PeerRecord]) -> Result<()> {
    let view = TopologyView::from_peers(peers);
    let wg_summary = wg::interface_summary().ok();

    let mut regions: Vec<&Region> = view.regions();
    regions.sort();

    for region in &regions {
        let region_peers = view.peers_in_region(region);
        ui::heading(&format!(
            "{} ({} nodes)",
            region.as_str(),
            region_peers.len()
        ));

        let mut zones: Vec<&Zone> = view.zones_in_region(region);
        zones.sort();

        for zone in &zones {
            let zone_peers = view.peers_in_zone(zone);
            ui::info_line(zone.as_str(), &format!("{} nodes", zone_peers.len()));

            let mut sorted_peers: Vec<&PeerRecord> = zone_peers.iter().collect();
            sorted_peers.sort_by(|a, b| a.name.cmp(&b.name));

            for peer in sorted_peers {
                let status = match peer.status {
                    PeerStatus::Active => "active",
                    PeerStatus::Unreachable => "unreach",
                    PeerStatus::Removed => "removed",
                };

                let traffic_str = if let Some(ref summary) = wg_summary {
                    match summary
                        .peers
                        .iter()
                        .find(|p| p.public_key == peer.wg_public_key)
                    {
                        Some(wg_peer) => format_traffic(wg_peer.rx_bytes, wg_peer.tx_bytes),
                        None => "-".into(),
                    }
                } else {
                    "-".into()
                };

                println!(
                    "    {:<18} {:<10} {:<24} {}",
                    truncate(&sanitize(&peer.name), 17),
                    status,
                    peer.endpoint,
                    traffic_str,
                );
            }
        }
    }

    Ok(())
}

/// Filter peers by region and/or zone string filters.
fn filter_peers(
    peers: Vec<PeerRecord>,
    region: Option<&str>,
    zone: Option<&str>,
) -> Vec<PeerRecord> {
    let region_filter = region.and_then(Region::new);
    let zone_filter = zone.and_then(Zone::new);

    if region_filter.is_none() && zone_filter.is_none() {
        return peers;
    }

    let view = TopologyView::from_peers(&peers);

    // Zone filter is more specific; if both given, zone wins.
    if let Some(ref z) = zone_filter {
        return view.peers_in_zone(z).to_vec();
    }

    if let Some(ref r) = region_filter {
        return view.peers_in_region(r).to_vec();
    }

    peers
}

/// Print a single peer row in the flat table format.
fn print_peer_row(peer: &PeerRecord, wg_summary: &Option<wg::InterfaceSummary>) {
    let status = match peer.status {
        PeerStatus::Active => "active",
        PeerStatus::Unreachable => "unreach",
        PeerStatus::Removed => "removed",
    };

    let (handshake_str, traffic_str) = if let Some(ref summary) = wg_summary {
        match summary
            .peers
            .iter()
            .find(|p| p.public_key == peer.wg_public_key)
        {
            Some(wg_peer) => {
                let hs = wg_peer
                    .last_handshake
                    .map(format_ago)
                    .unwrap_or_else(|| "never".into());
                let traffic = format_traffic(wg_peer.rx_bytes, wg_peer.tx_bytes);
                (hs, traffic)
            }
            None => ("n/a".into(), "-".into()),
        }
    } else {
        ("-".into(), "-".into())
    };

    let region = peer
        .region
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| "-".into());
    let zone = peer
        .zone
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| "-".into());

    let since_str = format_since(peer.last_seen);
    let mesh_ipv6 = peer.mesh_ipv6.to_string();

    println!(
        "{:<18} {:<12} {:<14} {:<40} {:<24} {:>8} {:<14} {:>12} {:>14}",
        truncate(&sanitize(&peer.name), 17),
        truncate(&region, 11),
        truncate(&zone, 13),
        truncate(&mesh_ipv6, 39),
        peer.endpoint,
        status,
        since_str,
        handshake_str,
        traffic_str,
    );
}

/// Deduplicate peers by WireGuard public key, keeping the entry with the
/// latest `last_seen` timestamp. Returns a sorted vec (by name then key).
fn dedup_peers(peers: &[PeerRecord]) -> Vec<PeerRecord> {
    let mut by_key: HashMap<&str, &PeerRecord> = HashMap::new();
    for peer in peers {
        by_key
            .entry(peer.wg_public_key.as_str())
            .and_modify(|existing| {
                if peer.last_seen > existing.last_seen {
                    *existing = peer;
                }
            })
            .or_insert(peer);
    }
    let mut result: Vec<PeerRecord> = by_key.into_values().cloned().collect();
    result.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.wg_public_key.cmp(&b.wg_public_key))
    });
    result
}

/// Format a `last_seen` epoch-seconds timestamp as a human-readable duration
/// (e.g. "2h 15m ago"). Returns "-" if the timestamp is zero.
fn format_since(epoch_secs: u64) -> String {
    if epoch_secs == 0 {
        return "-".into();
    }

    let seen_time = UNIX_EPOCH + Duration::from_secs(epoch_secs);
    let elapsed = SystemTime::now()
        .duration_since(seen_time)
        .unwrap_or_default()
        .as_secs();

    format_duration(elapsed)
}

fn format_ago(time: SystemTime) -> String {
    if time == UNIX_EPOCH {
        return "never".into();
    }

    let elapsed = SystemTime::now()
        .duration_since(time)
        .unwrap_or_default()
        .as_secs();

    format_duration(elapsed)
}

/// Format seconds into a human-readable relative duration string.
fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{m}m ago")
        } else {
            format!("{m}m {s}s ago")
        }
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h ago")
        } else {
            format!("{h}h {m}m ago")
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{d}d ago")
        } else {
            format!("{d}d {h}h ago")
        }
    }
}

fn format_traffic(rx: u64, tx: u64) -> String {
    if rx == 0 && tx == 0 {
        return "-".into();
    }
    format!("{}↓ {}↑", format_short(rx), format_short(tx))
}

fn format_short(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}K", bytes / 1024)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{}M", bytes / (1024 * 1024))
    } else {
        format!("{}G", bytes / (1024 * 1024 * 1024))
    }
}

#[derive(Serialize)]
struct PeerJson<'a> {
    name: &'a str,
    wg_public_key: &'a str,
    endpoint: String,
    mesh_ipv6: String,
    last_seen: u64,
    status: &'a str,
    region: Option<&'a str>,
    zone: Option<&'a str>,
}

use super::ui::truncate;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn make_peer(name: &str, key: &str, last_seen: u64) -> PeerRecord {
        PeerRecord {
            name: name.into(),
            wg_public_key: key.into(),
            endpoint: "127.0.0.1:51820".parse().unwrap(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1),
            last_seen,
            status: PeerStatus::Active,
            region: None,
            zone: None,
            topology: None,
        }
    }

    #[test]
    fn dedup_keeps_latest_last_seen() {
        let peers = vec![
            make_peer("node-a", "key-1", 100),
            make_peer("node-a", "key-1", 200),
            make_peer("node-a", "key-1", 150),
        ];
        let result = dedup_peers(&peers);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].last_seen, 200);
    }

    #[test]
    fn dedup_preserves_distinct_keys() {
        let peers = vec![
            make_peer("node-a", "key-1", 100),
            make_peer("node-b", "key-2", 200),
        ];
        let result = dedup_peers(&peers);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn dedup_empty() {
        let result = dedup_peers(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(0), "0s ago");
        assert_eq!(format_duration(45), "45s ago");
        assert_eq!(format_duration(59), "59s ago");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(60), "1m ago");
        assert_eq!(format_duration(90), "1m 30s ago");
        assert_eq!(format_duration(3599), "59m 59s ago");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3600), "1h ago");
        assert_eq!(format_duration(8100), "2h 15m ago");
        assert_eq!(format_duration(86399), "23h 59m ago");
    }

    #[test]
    fn format_duration_days() {
        assert_eq!(format_duration(86400), "1d ago");
        assert_eq!(format_duration(90000), "1d 1h ago");
    }

    #[test]
    fn format_since_zero_returns_dash() {
        assert_eq!(format_since(0), "-");
    }

    #[test]
    fn format_short_bytes() {
        assert_eq!(format_short(0), "0B");
        assert_eq!(format_short(512), "512B");
        assert_eq!(format_short(1024), "1K");
        assert_eq!(format_short(1048576), "1M");
        assert_eq!(format_short(1073741824), "1G");
    }

    #[test]
    fn format_traffic_zero() {
        assert_eq!(format_traffic(0, 0), "-");
    }

    #[test]
    fn format_traffic_nonzero() {
        assert_eq!(format_traffic(1200, 3400), "1K↓ 3K↑");
    }

    fn make_peer_with_region(name: &str, key: &str, region: &str, zone: &str) -> PeerRecord {
        use syfrah_core::mesh::Topology;
        PeerRecord {
            name: name.into(),
            wg_public_key: key.into(),
            endpoint: "127.0.0.1:51820".parse().unwrap(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1),
            last_seen: 100,
            status: PeerStatus::Active,
            region: Some(region.into()),
            zone: Some(zone.into()),
            topology: Some(Topology {
                region: Region::new(region).unwrap(),
                zone: Zone::new(zone).unwrap(),
            }),
        }
    }

    #[test]
    fn filter_peers_no_filter_returns_all() {
        let peers = vec![
            make_peer_with_region("n1", "k1", "eu-west", "par-ovh"),
            make_peer_with_region("n2", "k2", "us-east", "nyc-1"),
        ];
        let result = filter_peers(peers.clone(), None, None);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_peers_by_region() {
        let peers = vec![
            make_peer_with_region("n1", "k1", "eu-west", "par-ovh"),
            make_peer_with_region("n2", "k2", "eu-west", "par-scw"),
            make_peer_with_region("n3", "k3", "us-east", "nyc-1"),
        ];
        let result = filter_peers(peers, Some("eu-west"), None);
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .all(|p| p.region.as_deref() == Some("eu-west")));
    }

    #[test]
    fn filter_peers_by_zone() {
        let peers = vec![
            make_peer_with_region("n1", "k1", "eu-west", "par-ovh"),
            make_peer_with_region("n2", "k2", "eu-west", "par-scw"),
            make_peer_with_region("n3", "k3", "us-east", "nyc-1"),
        ];
        let result = filter_peers(peers, None, Some("par-ovh"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "n1");
    }

    #[test]
    fn filter_peers_zone_takes_precedence() {
        let peers = vec![
            make_peer_with_region("n1", "k1", "eu-west", "par-ovh"),
            make_peer_with_region("n2", "k2", "eu-west", "par-scw"),
        ];
        // When both region and zone are given, zone wins
        let result = filter_peers(peers, Some("eu-west"), Some("par-ovh"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "n1");
    }

    #[test]
    fn filter_peers_invalid_region_returns_empty() {
        let peers = vec![make_peer_with_region("n1", "k1", "eu-west", "par-ovh")];
        let result = filter_peers(peers, Some("nonexistent"), None);
        assert!(result.is_empty());
    }
}
