//! Criterion benchmarks for syfrah-fabric operations.
//!
//! Covers TopologyView construction, diff_peers, sanitize, and store upsert.

use std::net::{Ipv6Addr, SocketAddr};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use syfrah_core::mesh::{PeerRecord, PeerStatus, Region, Topology, Zone};
use syfrah_fabric::sanitize::sanitize;
use syfrah_fabric::topology::TopologyView;
use syfrah_fabric::wg::{diff_peers, PeerSummary};
use wireguard_control::Key;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_peer(i: usize, region: &str, zone: &str) -> PeerRecord {
    PeerRecord {
        name: format!("node-{i}"),
        wg_public_key: format!("key-{i:040}"),
        endpoint: SocketAddr::new(
            std::net::IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i as u16)),
            51820,
        ),
        mesh_ipv6: Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i as u16),
        last_seen: 1_700_000_000,
        status: PeerStatus::Active,
        region: Some(region.to_owned()),
        zone: Some(zone.to_owned()),
        topology: Some(Topology {
            region: Region::new(region).unwrap(),
            zone: Zone::new(zone).unwrap(),
        }),
        hypervisor_report: None,
    }
}

fn make_peer_list(n: usize) -> Vec<PeerRecord> {
    let regions = ["eu-west", "us-east", "ap-south"];
    let zones = [
        "eu-west-1a",
        "eu-west-1b",
        "us-east-1a",
        "us-east-1b",
        "ap-south-1a",
    ];
    (0..n)
        .map(|i| make_peer(i, regions[i % regions.len()], zones[i % zones.len()]))
        .collect()
}

// ---------------------------------------------------------------------------
// TopologyView construction
// ---------------------------------------------------------------------------

fn bench_topology_view(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology_view_construction");

    for size in [100, 1000, 5000] {
        let peers = make_peer_list(size);

        group.bench_with_input(BenchmarkId::new("from_peers", size), &peers, |b, peers| {
            b.iter(|| TopologyView::from_peers(black_box(peers)));
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// diff_peers
// ---------------------------------------------------------------------------

fn bench_diff_peers(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_peers");

    for size in [100, 1000, 5000] {
        let peers = make_peer_list(size);
        // Simulate WG peers that match desired state (no diff scenario)
        let wg_peers: Vec<PeerSummary> = peers
            .iter()
            .map(|p| PeerSummary {
                public_key: p.wg_public_key.clone(),
                endpoint: Some(p.endpoint),
                allowed_ips: vec![format!("{}/128", p.mesh_ipv6)],
                last_handshake: None,
                rx_bytes: 0,
                tx_bytes: 0,
            })
            .collect();
        let self_key = Key::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap();

        group.bench_with_input(
            BenchmarkId::new("no_change", size),
            &(&peers, &wg_peers, &self_key),
            |b, (peers, wg_peers, self_key)| {
                b.iter(|| diff_peers(black_box(self_key), black_box(peers), black_box(wg_peers)));
            },
        );

        // Scenario where all peers are missing from WG (worst case)
        let empty_wg: Vec<PeerSummary> = Vec::new();

        group.bench_with_input(
            BenchmarkId::new("all_missing", size),
            &(&peers, &empty_wg, &self_key),
            |b, (peers, wg_peers, self_key)| {
                b.iter(|| diff_peers(black_box(self_key), black_box(peers), black_box(wg_peers)));
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// sanitize
// ---------------------------------------------------------------------------

fn bench_sanitize(c: &mut Criterion) {
    let clean = "my-node-01.prod";
    let dirty = "evil\n\x1b[31mRED\x1b[0m\rstuff\t\0end";
    let long = "a".repeat(500);

    c.bench_function("sanitize_clean_input", |b| {
        b.iter(|| sanitize(black_box(clean)));
    });

    c.bench_function("sanitize_dirty_input", |b| {
        b.iter(|| sanitize(black_box(dirty)));
    });

    c.bench_function("sanitize_long_input", |b| {
        b.iter(|| sanitize(black_box(&long)));
    });
}

// ---------------------------------------------------------------------------
// Store upsert (using LayerDb directly with a temp file)
// ---------------------------------------------------------------------------

fn bench_store_upsert(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench.redb");
    let db = syfrah_state::LayerDb::open_at(&db_path).expect("open db");

    let peer = make_peer(1, "eu-west", "eu-west-1a");

    c.bench_function("store_upsert_peer", |b| {
        b.iter(|| {
            db.set("peers", &peer.wg_public_key, black_box(&peer))
                .unwrap();
        });
    });

    // Pre-populate 100 peers, then benchmark upsert of a new peer
    for i in 0..100 {
        let p = make_peer(i + 1000, "eu-west", "eu-west-1a");
        db.set("peers", &p.wg_public_key, &p).unwrap();
    }

    let new_peer = make_peer(9999, "us-east", "us-east-1a");

    c.bench_function("store_upsert_peer_100_existing", |b| {
        b.iter(|| {
            db.set("peers", &new_peer.wg_public_key, black_box(&new_peer))
                .unwrap();
        });
    });

    // Keep dir alive until end of benchmark
    drop(db);
    drop(dir);
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_topology_view,
    bench_diff_peers,
    bench_sanitize,
    bench_store_upsert,
);
criterion_main!(benches);
