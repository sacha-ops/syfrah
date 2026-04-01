//! Integration tests for store atomicity under concurrent access.
//!
//! These tests validate that `upsert_peer()` and `get_peers()` behave correctly
//! when called from multiple threads simultaneously. They override the HOME
//! environment variable to isolate each test in a temporary directory.
//!
//! Note: redb does not allow multiple `Database` handles on the same file within
//! one process (`DatabaseAlreadyOpen` error). The store module opens a fresh handle
//! per call, so truly simultaneous calls from different threads will contend.
//! These tests use retry loops to exercise the concurrent-access path.

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use syfrah_core::mesh::{PeerRecord, PeerStatus};
use syfrah_fabric::store;

/// Global mutex to serialize tests that modify the HOME env var.
/// Cargo test runs tests in parallel within a single process, so
/// we must ensure only one test touches HOME at a time.
static HOME_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

/// Build a test PeerRecord with a unique name and WG key.
fn make_test_peer(name: &str, index: usize) -> PeerRecord {
    PeerRecord {
        name: name.to_string(),
        wg_public_key: format!("wg_pub_key_{name}_{index}"),
        endpoint: SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, index as u8)),
            51820,
        ),
        mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, index as u16),
        last_seen: 1000 + index as u64,
        status: PeerStatus::Active,
        region: Some("us-east-1".into()),
        zone: Some(format!("us-east-1-zone-{index}")),
        topology: None,
        hypervisor_report: None,
    }
}

/// Build a minimal NodeState so that `load()` and `save()` work.
fn make_node_state() -> store::NodeState {
    store::NodeState {
        mesh_name: "test-mesh".into(),
        mesh_secret: "syf_sk_test_secret".into(),
        wg_private_key: "test_wg_private".into(),
        wg_public_key: "test_wg_public".into(),
        mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1),
        mesh_prefix: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 0),
        wg_listen_port: 51820,
        node_name: "test-node".into(),
        public_endpoint: None,
        peering_port: 51821,
        peers: vec![],
        region: Some("us-east-1".into()),
        zone: Some("us-east-1-zone-1".into()),
        metrics: Default::default(),
    }
}

/// Run a test body with HOME set to a fresh temp directory.
/// The HOME_LOCK ensures only one test modifies the env at a time.
fn with_temp_home<F: FnOnce()>(f: F) {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let old_home = std::env::var("HOME").ok();

    unsafe { std::env::set_var("HOME", tmp.path()) };

    // Initialize the store with a base NodeState so redb exists
    store::save(&make_node_state()).unwrap();

    f();

    // Restore HOME
    match old_home {
        Some(h) => unsafe { std::env::set_var("HOME", h) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    // tmp is dropped here, cleaning up the directory
}

/// Retry an upsert_peer call, handling transient DatabaseAlreadyOpen errors
/// that occur when multiple threads try to open the same redb file.
fn upsert_peer_with_retry(peer: &PeerRecord, max_retries: usize) {
    for attempt in 0..max_retries {
        match store::upsert_peer(peer) {
            Ok(()) => return,
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("already open") && attempt < max_retries - 1 {
                    // Brief sleep with jitter to reduce contention
                    std::thread::sleep(std::time::Duration::from_millis(10 + (attempt as u64 * 5)));
                    continue;
                }
                panic!("upsert_peer failed after {attempt} retries: {e}");
            }
        }
    }
}

/// Retry a get_peers call, handling transient DatabaseAlreadyOpen errors.
fn get_peers_with_retry(max_retries: usize) -> Vec<PeerRecord> {
    for attempt in 0..max_retries {
        match store::get_peers() {
            Ok(peers) => return peers,
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("already open") && attempt < max_retries - 1 {
                    std::thread::sleep(std::time::Duration::from_millis(10 + (attempt as u64 * 5)));
                    continue;
                }
                panic!("get_peers failed after {attempt} retries: {e}");
            }
        }
    }
    unreachable!()
}

// ── Test 1: 10 concurrent upsert_peer() — all persisted ────

#[test]
fn concurrent_upsert_all_persisted() {
    with_temp_home(|| {
        let thread_count = 10;
        let barrier = Arc::new(Barrier::new(thread_count));

        let handles: Vec<_> = (0..thread_count)
            .map(|i| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let peer = make_test_peer(&format!("peer-{i}"), i);
                    barrier.wait(); // All threads start simultaneously
                    upsert_peer_with_retry(&peer, 50);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let peers = store::get_peers().unwrap();
        assert_eq!(
            peers.len(),
            thread_count,
            "Expected {thread_count} peers after concurrent upserts, got {}",
            peers.len()
        );

        // Verify each peer is present with correct data
        for i in 0..thread_count {
            let key = format!("wg_pub_key_peer-{i}_{i}");
            assert!(
                peers.iter().any(|p| p.wg_public_key == key),
                "Missing peer with key {key}"
            );
        }
    });
}

// ── Test 2: Concurrent upsert + read — no panic ────────────

#[test]
fn concurrent_upsert_and_read_no_panic() {
    with_temp_home(|| {
        let thread_count = 10;
        let writers = 5;
        let readers = 5;
        let barrier = Arc::new(Barrier::new(thread_count));
        let read_results: Arc<Mutex<Vec<Vec<PeerRecord>>>> = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();

        // Writer threads
        for i in 0..writers {
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                let peer = make_test_peer(&format!("writer-{i}"), i);
                barrier.wait();
                upsert_peer_with_retry(&peer, 50);
            }));
        }

        // Reader threads
        for _ in 0..readers {
            let barrier = barrier.clone();
            let results = read_results.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                // Read multiple times while writers are active
                for _ in 0..5 {
                    let peers = get_peers_with_retry(50);
                    results.lock().unwrap().push(peers);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Final state must have all writer peers
        let final_peers = store::get_peers().unwrap();
        assert_eq!(final_peers.len(), writers);

        // All intermediate reads must return valid (non-panicking) results
        let snapshots = read_results.lock().unwrap();
        assert!(
            !snapshots.is_empty(),
            "Readers should have captured some snapshots"
        );
        // Each snapshot should have between 0 and `writers` peers
        for snapshot in snapshots.iter() {
            assert!(
                snapshot.len() <= writers,
                "Snapshot has more peers than writers: {} > {writers}",
                snapshot.len()
            );
        }
    });
}

// ── Test 3: upsert_peer then load — consistency ────────────

#[test]
fn upsert_then_load_returns_peer() {
    with_temp_home(|| {
        let peer = make_test_peer("load-test", 42);
        store::upsert_peer(&peer).unwrap();

        // Verify via get_peers (redb path)
        let redb_peers = store::get_peers().unwrap();
        assert!(
            redb_peers
                .iter()
                .any(|p| p.wg_public_key == peer.wg_public_key),
            "Peer not found via get_peers() (redb path)"
        );

        // Verify via load() which tries redb first, falls back to JSON
        let state = store::load().unwrap();
        assert!(
            state
                .peers
                .iter()
                .any(|p| p.wg_public_key == peer.wg_public_key),
            "Peer not found via load() (redb/JSON path)"
        );
    });
}

// ── Test 4: Duplicate upsert updates, not duplicates ────────

#[test]
fn upsert_same_peer_twice_updates_not_duplicates() {
    with_temp_home(|| {
        let mut peer = make_test_peer("dup-test", 1);
        peer.status = PeerStatus::Active;
        store::upsert_peer(&peer).unwrap();

        // Update the same peer (same WG key) with new status
        peer.status = PeerStatus::Unreachable;
        store::upsert_peer(&peer).unwrap();

        let peers = store::get_peers().unwrap();
        let matching: Vec<_> = peers
            .iter()
            .filter(|p| p.wg_public_key == peer.wg_public_key)
            .collect();

        assert_eq!(
            matching.len(),
            1,
            "Expected exactly 1 peer after duplicate upsert, got {}",
            matching.len()
        );
        assert_eq!(
            matching[0].status,
            PeerStatus::Unreachable,
            "Peer status should be updated to Unreachable"
        );
    });
}

// ── Test 5: clear() removes all state — join works immediately after ──

#[test]
fn clear_removes_all_state_single_call() {
    with_temp_home(|| {
        // Precondition: state exists (save was called in with_temp_home)
        assert!(store::exists(), "state should exist before clear");

        // Add a peer and set a metric so both redb tables have data
        let peer = make_test_peer("pre-clear", 1);
        store::upsert_peer(&peer).unwrap();
        store::set_metric("daemon_started_at", 1234567890).unwrap();

        // A single clear() must wipe everything
        store::clear().unwrap();

        assert!(
            !store::exists(),
            "store::exists() must return false after clear()"
        );

        // Saving new state must succeed (simulates a fresh join)
        let new_state = make_node_state();
        store::save(&new_state).unwrap();

        assert!(store::exists(), "state should exist after re-save");

        // Verify that old peers and metrics are gone
        let peers = store::get_peers().unwrap();
        assert!(
            peers.is_empty(),
            "peers should be empty after clear + fresh save, got {}",
            peers.len()
        );

        let loaded = store::load().unwrap();
        assert_eq!(
            loaded.metrics.daemon_started_at, 0,
            "daemon_started_at should be 0 after clear + fresh save"
        );
    });
}

// ── Test 6: JSON/redb consistency after concurrent upserts ────

#[test]
fn json_and_redb_consistent_after_concurrent_upserts() {
    with_temp_home(|| {
        let thread_count = 10;
        let barrier = Arc::new(Barrier::new(thread_count));

        let handles: Vec<_> = (0..thread_count)
            .map(|i| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let peer = make_test_peer(&format!("consistency-{i}"), i);
                    barrier.wait();
                    upsert_peer_with_retry(&peer, 50);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Get peers from redb via get_peers()
        let redb_peers = store::get_peers().unwrap();

        // Get peers from load() (which reads redb, or falls back to JSON)
        let loaded_state = store::load().unwrap();

        // Both should have the same count
        assert_eq!(
            redb_peers.len(),
            loaded_state.peers.len(),
            "redb has {} peers but load() has {} peers",
            redb_peers.len(),
            loaded_state.peers.len()
        );

        // Both should have the same set of WG keys
        let mut redb_keys: Vec<_> = redb_peers.iter().map(|p| &p.wg_public_key).collect();
        let mut load_keys: Vec<_> = loaded_state
            .peers
            .iter()
            .map(|p| &p.wg_public_key)
            .collect();
        redb_keys.sort();
        load_keys.sort();
        assert_eq!(
            redb_keys, load_keys,
            "redb and load() returned different peer key sets"
        );

        // All 10 peers should be present
        assert_eq!(
            redb_peers.len(),
            thread_count,
            "Expected {thread_count} peers, got {}",
            redb_peers.len()
        );
    });
}

// ── Test 7: peer_count_and_exists returns correct values ────

#[test]
fn peer_count_and_exists_empty_store() {
    with_temp_home(|| {
        // No peers inserted yet — count should be 0, exists should be false
        let (count, exists) = store::peer_count_and_exists("nonexistent-key").unwrap();
        assert_eq!(count, 0, "expected 0 peers in fresh store");
        assert!(!exists, "nonexistent key should not exist");
    });
}

#[test]
fn peer_count_and_exists_after_upserts() {
    with_temp_home(|| {
        let peer1 = make_test_peer("pce-1", 1);
        let peer2 = make_test_peer("pce-2", 2);
        store::upsert_peer(&peer1).unwrap();
        store::upsert_peer(&peer2).unwrap();

        // Existing key
        let (count, exists) = store::peer_count_and_exists(&peer1.wg_public_key).unwrap();
        assert_eq!(count, 2, "expected 2 peers after two upserts");
        assert!(exists, "peer1 should exist");

        // Non-existing key
        let (count, exists) = store::peer_count_and_exists("no-such-key").unwrap();
        assert_eq!(count, 2, "count should still be 2");
        assert!(!exists, "non-existent key should return false");
    });
}

// ── Test: gc_removed_peers deletes only stale Removed peers ──

#[test]
fn gc_removed_peers_deletes_stale_removed() {
    with_temp_home(|| {
        // Active peer — should survive GC.
        let active = make_test_peer("gc-active", 1);
        store::upsert_peer(&active).unwrap();

        // Removed peer with ancient last_seen — should be collected.
        let mut stale_removed = make_test_peer("gc-stale", 2);
        stale_removed.status = PeerStatus::Removed;
        stale_removed.last_seen = 1000; // epoch + 1000s, well past any threshold
        store::upsert_peer(&stale_removed).unwrap();

        // Removed peer with recent last_seen — should survive.
        let mut fresh_removed = make_test_peer("gc-fresh", 3);
        fresh_removed.status = PeerStatus::Removed;
        fresh_removed.last_seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        store::upsert_peer(&fresh_removed).unwrap();

        assert_eq!(store::get_peers().unwrap().len(), 3);

        // GC with 24h threshold — only the stale removed peer qualifies.
        let collected = store::gc_removed_peers(86400).unwrap();
        assert_eq!(collected, 1, "expected 1 peer to be garbage-collected");

        let remaining = store::get_peers().unwrap();
        assert_eq!(remaining.len(), 2, "expected 2 peers after GC");
        assert!(
            remaining.iter().any(|p| p.name == "gc-active"),
            "active peer should survive"
        );
        assert!(
            remaining.iter().any(|p| p.name == "gc-fresh"),
            "recently-removed peer should survive"
        );
        assert!(
            !remaining.iter().any(|p| p.name == "gc-stale"),
            "stale removed peer should be deleted"
        );
    });
}

#[test]
fn gc_removed_peers_noop_when_threshold_zero() {
    with_temp_home(|| {
        let mut removed = make_test_peer("gc-zero", 1);
        removed.status = PeerStatus::Removed;
        removed.last_seen = 1000;
        store::upsert_peer(&removed).unwrap();

        let collected = store::gc_removed_peers(0).unwrap();
        assert_eq!(collected, 0, "threshold 0 should disable GC");
        assert_eq!(store::get_peers().unwrap().len(), 1);
    });
}
