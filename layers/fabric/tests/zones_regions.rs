//! Unit tests for zone/region generation and PeerRecord region/zone fields.

use syfrah_core::mesh::{PeerRecord, PeerStatus};
use syfrah_fabric::store::generate_zone;

fn make_peer(name: &str, region: Option<&str>, zone: Option<&str>) -> PeerRecord {
    PeerRecord {
        name: name.to_string(),
        wg_public_key: format!("key_{name}"),
        endpoint: "127.0.0.1:51820".parse().unwrap(),
        mesh_ipv6: "fd12::1".parse().unwrap(),
        last_seen: 0,
        status: PeerStatus::Active,
        region: region.map(|s| s.to_string()),
        zone: zone.map(|s| s.to_string()),
        topology: None,
        hypervisor_report: None,
    }
}

#[test]
fn generate_zone_first_node() {
    let zone = generate_zone("us-east", &[]);
    assert_eq!(zone, "zone-1");
}

#[test]
fn generate_zone_increments() {
    let peers = vec![
        make_peer("a", Some("us-east"), Some("zone-1")),
        make_peer("b", Some("us-east"), Some("zone-2")),
    ];
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-3");
}

#[test]
fn generate_zone_different_region_ignored() {
    let peers = vec![
        make_peer("a", Some("us-east"), Some("zone-1")),
        make_peer("b", Some("region-2"), Some("zone-1")),
    ];
    // Both zones parse as zone-1, max index = 1 → next = 2
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-2");
}

#[test]
fn generate_zone_with_gaps() {
    // Zone-1 and zone-3 exist but zone-2 was removed — still takes max+1
    let peers = vec![
        make_peer("a", Some("us-east"), Some("zone-1")),
        make_peer("c", Some("us-east"), Some("zone-3")),
    ];
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-4");
}

#[test]
fn generate_zone_no_matching_region() {
    let peers = vec![make_peer("a", Some("region-2"), Some("zone-5"))];
    // zone-5 is parsed, peer count = 1 → max(5,1)+1 = 6
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-6");
}

#[test]
fn generate_zone_peers_without_zone() {
    let peers = vec![
        make_peer("a", None, None),
        make_peer("b", Some("us-east"), None),
    ];
    // No zone prefixes found, but 2 peers → max(0,2)+1 = 3
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-3");
}

#[test]
fn generate_zone_custom_region_name() {
    let peers = vec![
        make_peer("a", Some("eu-west"), Some("zone-1")),
        make_peer("b", Some("eu-west"), Some("zone-2")),
    ];
    let zone = generate_zone("eu-west", &peers);
    assert_eq!(zone, "zone-3");
}

#[test]
fn generate_zone_legacy_format_backward_compat() {
    // Legacy peers with region-prefixed zone names should still be parsed
    let peers = vec![
        make_peer("a", Some("us-east"), Some("us-east-zone-1")),
        make_peer("b", Some("us-east"), Some("us-east-zone-2")),
    ];
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-3");
}

#[test]
fn generate_zone_mixed_legacy_and_new() {
    // Mix of old and new format zones
    let peers = vec![
        make_peer("a", Some("us-east"), Some("us-east-zone-3")),
        make_peer("b", Some("us-east"), Some("zone-5")),
    ];
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-6");
}

#[test]
fn peer_record_region_zone_serde() {
    let peer = make_peer("test", Some("eu-west"), Some("zone-1"));
    let json = serde_json::to_string(&peer).unwrap();
    assert!(json.contains("eu-west"));
    assert!(json.contains("zone-1"));

    let parsed: PeerRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.region, Some("eu-west".to_string()));
    assert_eq!(parsed.zone, Some("zone-1".to_string()));
}

#[test]
fn peer_record_backward_compat_no_region_zone() {
    // Old-format JSON without region/zone fields
    let json = r#"{
        "name": "old-node",
        "wg_public_key": "key123",
        "endpoint": "1.2.3.4:51820",
        "mesh_ipv6": "fd12::1",
        "last_seen": 0,
        "status": "Active"
    }"#;
    let parsed: PeerRecord = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.region, None);
    assert_eq!(parsed.zone, None);
    assert_eq!(parsed.name, "old-node");
}

#[test]
fn generate_zone_large_index() {
    let mut peers = Vec::new();
    for i in 1..=100 {
        peers.push(make_peer(
            &format!("n{i}"),
            Some("us-east"),
            Some(&format!("zone-{i}")),
        ));
    }
    let zone = generate_zone("us-east", &peers);
    assert_eq!(zone, "zone-101");
}
