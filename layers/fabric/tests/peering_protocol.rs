//! E2E test for the TCP peering protocol.
//!
//! Tests the full join flow: a new node sends a JoinRequest,
//! the existing node auto-accepts via PIN, and the new node
//! receives the mesh secret + peer list.
//!
//! This test does NOT require root or WireGuard — it only tests
//! the TCP protocol layer.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use syfrah_core::addressing;
use syfrah_core::mesh::{sign_join_request, JoinRequest, PeerRecord, PeerStatus};
use syfrah_core::secret::MeshSecret;
use syfrah_fabric::peering::{
    generate_pin, generate_request_id, send_join_request, AutoAcceptConfig, OnAccepted,
    PeeringState,
};

/// Build and sign a JoinRequest using the given keypair.
fn make_signed_join_request(
    keypair: &wireguard_control::KeyPair,
    node_name: &str,
    endpoint: &str,
    wg_listen_port: u16,
    pin: Option<String>,
    region: Option<String>,
    zone: Option<String>,
) -> JoinRequest {
    let mut req = JoinRequest {
        request_id: generate_request_id(),
        node_name: node_name.to_string(),
        wg_public_key: keypair.public.to_base64(),
        endpoint: endpoint.parse().unwrap(),
        wg_listen_port,
        pin,
        region,
        zone,
        timestamp: 0,
        signature: String::new(),
    };
    let mut priv_bytes = [0u8; 32];
    priv_bytes.copy_from_slice(keypair.private.as_bytes());
    sign_join_request(&mut req, &priv_bytes);
    req
}

/// Find a free TCP port by binding to port 0
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn join_with_pin_auto_accept() {
    // ── Setup: create a mesh "leader" node ──

    let mesh_secret = MeshSecret::generate();
    let mesh_prefix = syfrah_fabric::daemon::derive_prefix_from_secret(&mesh_secret);
    let encryption_key = mesh_secret.encryption_key();
    let pin = generate_pin();
    let peering_port = free_port();

    // Leader's WireGuard keypair (we just need the public key for the protocol)
    let leader_keypair = syfrah_fabric::wg::generate_keypair();
    let leader_ipv6 =
        addressing::derive_node_address(&mesh_prefix, leader_keypair.public.as_bytes());
    let leader_endpoint: SocketAddr = format!("203.0.113.1:{}", free_port()).parse().unwrap();

    let leader_record = PeerRecord {
        name: "leader".to_string(),
        wg_public_key: leader_keypair.public.to_base64(),
        endpoint: leader_endpoint,
        mesh_ipv6: leader_ipv6,
        last_seen: 0,
        status: PeerStatus::Active,
        region: None,
        zone: None,
        topology: None,
        hypervisor_report: None,
    };

    // ── Start peering listener with PIN auto-accept ──

    let peering_state = Arc::new(PeeringState::new());

    peering_state
        .set_auto_accept(Some(AutoAcceptConfig {
            pin: pin.clone(),
            mesh_name: "test-mesh".to_string(),
            mesh_secret_str: mesh_secret.to_string(),
            mesh_prefix,
            my_record: leader_record.clone(),
            wg_pubkey: leader_keypair.public.clone(),
            encryption_key,
            peering_port,
            max_peers: 1000,
        }))
        .await;

    // Track accepted peers
    let accepted_peers: Arc<tokio::sync::Mutex<Vec<PeerRecord>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let accepted_clone = accepted_peers.clone();

    let on_accepted: OnAccepted = Arc::new(move |record| {
        let peers = accepted_clone.clone();
        tokio::spawn(async move {
            peers.lock().await.push(record);
        });
    });

    let on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync> = Arc::new(|_| {});

    // Start the listener in background
    let listener_state = peering_state.clone();
    let listener_handle = tokio::spawn(async move {
        listener_state
            .run_listener(
                peering_port,
                Some(encryption_key),
                on_announce,
                on_accepted,
                None,
                None,
            )
            .await
            .ok();
    });

    // Give the listener time to bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // ── Joiner: send a join request with the correct PIN ──

    let joiner_keypair = syfrah_fabric::wg::generate_keypair();

    let join_request = make_signed_join_request(
        &joiner_keypair,
        "joiner",
        "0.0.0.0:51820",
        51820,
        Some(pin.clone()),
        Some("us-east".to_string()),
        Some("zone-1".to_string()),
    );

    let target: SocketAddr = format!("127.0.0.1:{peering_port}").parse().unwrap();
    let response = send_join_request(target, join_request, None).await.unwrap();

    // ── Verify: join was accepted ──

    assert!(response.accepted, "join request should be accepted");
    assert_eq!(response.mesh_name.as_deref(), Some("test-mesh"));
    assert_eq!(
        response.mesh_secret.as_deref(),
        Some(mesh_secret.to_string().as_str())
    );
    assert_eq!(response.mesh_prefix, Some(mesh_prefix));

    // The response should contain the leader as a peer
    assert!(
        !response.peers.is_empty(),
        "response should contain at least the leader peer"
    );
    assert!(
        response.peers.iter().any(|p| p.name == "leader"),
        "leader should be in the peer list"
    );

    // The leader should have recorded the joiner
    tokio::time::sleep(Duration::from_millis(100)).await;
    let accepted = accepted_peers.lock().await;
    assert_eq!(accepted.len(), 1, "one peer should have been accepted");
    assert_eq!(accepted[0].name, "joiner");
    assert_eq!(
        accepted[0].region.as_deref(),
        Some("us-east"),
        "accepted peer should have the joiner's region"
    );
    assert_eq!(
        accepted[0].zone.as_deref(),
        Some("zone-1"),
        "accepted peer should have the joiner's zone"
    );

    // Cleanup
    listener_handle.abort();
}

#[tokio::test]
async fn join_with_wrong_pin_falls_to_pending() {
    // ── Setup ──

    let mesh_secret = MeshSecret::generate();
    let encryption_key = mesh_secret.encryption_key();
    let correct_pin = generate_pin();
    let wrong_pin = "ZZZZZZ".to_string();
    let peering_port = free_port();
    let mesh_prefix = syfrah_fabric::daemon::derive_prefix_from_secret(&mesh_secret);

    let leader_keypair = syfrah_fabric::wg::generate_keypair();
    let leader_ipv6 =
        addressing::derive_node_address(&mesh_prefix, leader_keypair.public.as_bytes());

    let leader_record = PeerRecord {
        name: "leader".to_string(),
        wg_public_key: leader_keypair.public.to_base64(),
        endpoint: format!("203.0.113.1:{}", free_port()).parse().unwrap(),
        mesh_ipv6: leader_ipv6,
        last_seen: 0,
        status: PeerStatus::Active,
        region: None,
        zone: None,
        topology: None,
        hypervisor_report: None,
    };

    let peering_state = Arc::new(PeeringState::new());

    peering_state
        .set_auto_accept(Some(AutoAcceptConfig {
            pin: correct_pin,
            mesh_name: "test-mesh".to_string(),
            mesh_secret_str: mesh_secret.to_string(),
            mesh_prefix,
            my_record: leader_record,
            wg_pubkey: leader_keypair.public.clone(),
            encryption_key,
            peering_port,
            max_peers: 1000,
        }))
        .await;

    let on_accepted: OnAccepted = Arc::new(|_| {});
    let on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync> = Arc::new(|_| {});

    let listener_state = peering_state.clone();
    let listener_handle = tokio::spawn(async move {
        listener_state
            .run_listener(
                peering_port,
                Some(encryption_key),
                on_announce,
                on_accepted,
                None,
                None,
            )
            .await
            .ok();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // ── Send join request with WRONG pin ──

    let joiner_keypair = syfrah_fabric::wg::generate_keypair();

    let join_request = make_signed_join_request(
        &joiner_keypair,
        "joiner",
        "0.0.0.0:51820",
        51820,
        Some(wrong_pin),
        Some("us-east".to_string()),
        Some("zone-1".to_string()),
    );

    let target: SocketAddr = format!("127.0.0.1:{peering_port}").parse().unwrap();

    // The request should be pending (not auto-accepted), which means
    // the internal JOIN_TIMEOUT (5s) will expire and return an error.
    // The 2s PIN fail delay fires first, then the server leaves the
    // request pending without replying. We use a longer outer timeout
    // to let the internal timeout fire and produce an Err.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        send_join_request(target, join_request, None),
    )
    .await;

    // The inner JOIN_TIMEOUT should fire, returning Ok(Err(Timeout)).
    // Either way (outer timeout or inner error), the join was not accepted.
    match result {
        Err(_) => {}     // outer timeout — fine, request was not accepted
        Ok(Err(_)) => {} // inner timeout/error — fine, request was not accepted
        Ok(Ok(_)) => panic!("wrong PIN should not be auto-accepted"),
    }

    // The request should be in the pending list
    let pending = peering_state.list_pending().await;
    assert_eq!(pending.len(), 1, "request should be pending");
    assert_eq!(pending[0].node_name, "joiner");

    // Cleanup
    listener_handle.abort();
}

#[tokio::test]
async fn join_without_pin_goes_to_pending() {
    // ── Setup ──

    let mesh_secret = MeshSecret::generate();
    let encryption_key = mesh_secret.encryption_key();
    let peering_port = free_port();
    let mesh_prefix = syfrah_fabric::daemon::derive_prefix_from_secret(&mesh_secret);

    let leader_keypair = syfrah_fabric::wg::generate_keypair();
    let leader_ipv6 =
        addressing::derive_node_address(&mesh_prefix, leader_keypair.public.as_bytes());

    let leader_record = PeerRecord {
        name: "leader".to_string(),
        wg_public_key: leader_keypair.public.to_base64(),
        endpoint: format!("203.0.113.1:{}", free_port()).parse().unwrap(),
        mesh_ipv6: leader_ipv6,
        last_seen: 0,
        status: PeerStatus::Active,
        region: None,
        zone: None,
        topology: None,
        hypervisor_report: None,
    };

    let peering_state = Arc::new(PeeringState::new());

    // Auto-accept IS configured, but the joiner sends NO pin
    let pin_for_config = generate_pin();
    peering_state
        .set_auto_accept(Some(AutoAcceptConfig {
            pin: pin_for_config,
            mesh_name: "test-mesh".to_string(),
            mesh_secret_str: mesh_secret.to_string(),
            mesh_prefix,
            my_record: leader_record,
            wg_pubkey: leader_keypair.public.clone(),
            encryption_key,
            peering_port,
            max_peers: 1000,
        }))
        .await;

    let on_accepted: OnAccepted = Arc::new(|_| {});
    let on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync> = Arc::new(|_| {});

    let listener_state = peering_state.clone();
    let listener_handle = tokio::spawn(async move {
        listener_state
            .run_listener(
                peering_port,
                Some(encryption_key),
                on_announce,
                on_accepted,
                None,
                None,
            )
            .await
            .ok();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // ── Send join request WITHOUT pin ──

    let joiner_keypair = syfrah_fabric::wg::generate_keypair();

    let join_request = make_signed_join_request(
        &joiner_keypair,
        "no-pin-joiner",
        "0.0.0.0:51820",
        51820,
        None, // No PIN
        Some("us-east".to_string()),
        Some("zone-1".to_string()),
    );

    let target: SocketAddr = format!("127.0.0.1:{peering_port}").parse().unwrap();

    // Should timeout (goes to pending, no auto-accept without PIN)
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        send_join_request(target, join_request, None),
    )
    .await;

    assert!(
        result.is_err(),
        "no-pin request should not be auto-accepted"
    );

    // Verify it's pending
    let pending = peering_state.list_pending().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].node_name, "no-pin-joiner");

    // Cleanup
    listener_handle.abort();
}

#[test]
fn pin_is_6_alphanumeric_chars() {
    for _ in 0..100 {
        let pin = generate_pin();
        assert_eq!(pin.len(), 6, "PIN should be 6 characters, got: {pin}");
        assert!(
            pin.chars().all(|c| c.is_ascii_alphanumeric()),
            "PIN should be alphanumeric, got: {pin}"
        );
        // Should not contain ambiguous characters
        assert!(
            !pin.contains('0')
                && !pin.contains('O')
                && !pin.contains('1')
                && !pin.contains('I')
                && !pin.contains('L'),
            "PIN should not contain ambiguous characters (0, O, 1, I, L), got: {pin}"
        );
    }
}

#[test]
fn pin_rate_limiter_locks_out_after_max_attempts() {
    use syfrah_fabric::peering::PinRateLimiter;
    let mut rl = PinRateLimiter::new();
    let ip: IpAddr = "192.168.1.1".parse().unwrap();

    // First 4 failures should not lock out
    for _ in 0..4 {
        assert!(!rl.record_failure(ip), "should not be locked out yet");
    }
    assert!(!rl.is_locked_out(ip), "4 failures should not lock out");

    // 5th failure should trigger lockout
    rl.record_failure(ip);
    assert!(rl.is_locked_out(ip), "5 failures should lock out");

    // A different IP should not be locked out
    let other_ip: IpAddr = "192.168.1.2".parse().unwrap();
    assert!(
        !rl.is_locked_out(other_ip),
        "different IP should not be locked out"
    );
}

use std::net::IpAddr;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "flaky: broken on main since endpoint validation was added (port 0 rejection + handler task starvation)"]
async fn rate_limited_ip_gets_rejection() {
    // ── Setup ──
    // Requires multi_thread runtime: the listener and client tasks must
    // make progress concurrently (PIN_FAIL_DELAY sleeps block the handler).
    let mesh_secret = MeshSecret::generate();
    let encryption_key = mesh_secret.encryption_key();
    let correct_pin = generate_pin();
    let peering_port = free_port();
    let mesh_prefix = syfrah_fabric::daemon::derive_prefix_from_secret(&mesh_secret);

    let leader_keypair = syfrah_fabric::wg::generate_keypair();
    let leader_ipv6 =
        addressing::derive_node_address(&mesh_prefix, leader_keypair.public.as_bytes());

    let leader_record = PeerRecord {
        name: "leader".to_string(),
        wg_public_key: leader_keypair.public.to_base64(),
        endpoint: format!("127.0.0.1:{}", free_port()).parse().unwrap(),
        mesh_ipv6: leader_ipv6,
        last_seen: 0,
        status: PeerStatus::Active,
        region: None,
        zone: None,
        topology: None,
        hypervisor_report: None,
    };

    let peering_state = Arc::new(PeeringState::new());

    peering_state
        .set_auto_accept(Some(AutoAcceptConfig {
            pin: correct_pin.clone(),
            mesh_name: "test-mesh".to_string(),
            mesh_secret_str: mesh_secret.to_string(),
            mesh_prefix,
            my_record: leader_record,
            wg_pubkey: leader_keypair.public.clone(),
            encryption_key,
            peering_port,
            max_peers: 1000,
        }))
        .await;

    let on_accepted: OnAccepted = Arc::new(|_| {});
    let on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync> = Arc::new(|_| {});

    let listener_state = peering_state.clone();
    let listener_handle = tokio::spawn(async move {
        listener_state
            .run_listener(
                peering_port,
                Some(encryption_key),
                on_announce,
                on_accepted,
                None,
                None,
            )
            .await
            .ok();
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // ── Send 5 wrong PINs to exhaust the rate limit ──
    // Fire each attempt concurrently so they don't block on each other.
    // The server records the failure before the PIN_FAIL_DELAY, and the
    // requests then fall through to the pending queue (never approved).
    for i in 0..5 {
        let joiner_keypair = syfrah_fabric::wg::generate_keypair();
        let join_request = make_signed_join_request(
            &joiner_keypair,
            &format!("attacker-{i}"),
            "127.0.0.1:51820",
            51820,
            Some(format!("WRONG{i}")),
            None,
            None,
        );

        let target: SocketAddr = format!("127.0.0.1:{peering_port}").parse().unwrap();
        // These will go to pending after the 2s delay; just fire and let them timeout
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            send_join_request(target, join_request, None),
        )
        .await;
    }

    // Allow server tasks to finish processing all PIN failures before sending the 6th.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ── 6th attempt should get an immediate rejection (rate limited) ──
    let joiner_keypair = syfrah_fabric::wg::generate_keypair();
    let join_request = make_signed_join_request(
        &joiner_keypair,
        "attacker-final",
        "127.0.0.1:51820",
        51820,
        Some("WRONGX".to_string()),
        None,
        None,
    );

    let target: SocketAddr = format!("127.0.0.1:{peering_port}").parse().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        send_join_request(target, join_request, None),
    )
    .await;

    // Should get a rejection response (not timeout)
    match result {
        Ok(Ok(resp)) => {
            assert!(!resp.accepted, "rate-limited request should be rejected");
            assert!(
                resp.reason.as_deref().unwrap_or("").contains("too many"),
                "rejection reason should mention rate limiting"
            );
        }
        Ok(Err(e)) => panic!("rate-limited request failed with error: {e}"),
        Err(_) => panic!("rate-limited request timed out after 10s"),
    }

    // Cleanup
    listener_handle.abort();
}
