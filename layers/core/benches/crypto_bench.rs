//! Criterion benchmarks for syfrah-core crypto operations.
//!
//! Covers AES-GCM encrypt/decrypt, HKDF key derivation, and peer list
//! iteration at various scales.

use std::net::{Ipv6Addr, SocketAddr};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use syfrah_core::mesh::{
    decrypt_record, encrypt_record, PeerRecord, PeerStatus, Region, Topology, Zone,
};
use syfrah_core::secret::MeshSecret;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_peer(i: usize) -> PeerRecord {
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
        region: Some("eu-west".to_owned()),
        zone: Some("eu-west-1a".to_owned()),
        topology: Some(Topology {
            region: Region::new("eu-west").unwrap(),
            zone: Zone::new("eu-west-1a").unwrap(),
        }),
        hypervisor_report: None,
    }
}

fn make_peer_list(n: usize) -> Vec<PeerRecord> {
    (0..n).map(make_peer).collect()
}

// ---------------------------------------------------------------------------
// AES-GCM encrypt / decrypt
// ---------------------------------------------------------------------------

fn bench_aes_gcm(c: &mut Criterion) {
    let secret = MeshSecret::generate();
    let key = secret.encryption_key();
    let peer = make_peer(1);

    let encrypted = encrypt_record(&peer, &key).expect("encrypt");

    c.bench_function("aes_gcm_encrypt_record", |b| {
        b.iter(|| encrypt_record(black_box(&peer), black_box(&key)).unwrap());
    });

    c.bench_function("aes_gcm_decrypt_record", |b| {
        b.iter(|| decrypt_record(black_box(&encrypted), black_box(&key)).unwrap());
    });
}

// ---------------------------------------------------------------------------
// HKDF key derivation
// ---------------------------------------------------------------------------

fn bench_hkdf(c: &mut Criterion) {
    let secret_v2 = MeshSecret::from_bytes_v2([0xAB; 32]);
    let secret_v1 = MeshSecret::from_bytes([0xAB; 32]);

    c.bench_function("hkdf_v2_encryption_key", |b| {
        b.iter(|| black_box(&secret_v2).encryption_key());
    });

    c.bench_function("hkdf_v2_mesh_id", |b| {
        b.iter(|| black_box(&secret_v2).mesh_id());
    });

    c.bench_function("sha256_v1_encryption_key", |b| {
        b.iter(|| black_box(&secret_v1).encryption_key());
    });
}

// ---------------------------------------------------------------------------
// Peer list iteration
// ---------------------------------------------------------------------------

fn bench_peer_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("peer_list_iteration");

    for size in [100, 1000, 5000] {
        let peers = make_peer_list(size);

        group.bench_with_input(
            BenchmarkId::new("count_active", size),
            &peers,
            |b, peers| {
                b.iter(|| {
                    peers
                        .iter()
                        .filter(|p| p.status == PeerStatus::Active)
                        .count()
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("collect_public_keys", size),
            &peers,
            |b, peers| {
                b.iter(|| {
                    peers
                        .iter()
                        .map(|p| p.wg_public_key.as_str())
                        .collect::<Vec<_>>()
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

criterion_group!(benches, bench_aes_gcm, bench_hkdf, bench_peer_iteration);
criterion_main!(benches);
