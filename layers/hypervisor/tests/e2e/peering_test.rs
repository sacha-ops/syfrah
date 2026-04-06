//! Integration test: peering protocol over TLS.
//!
//! Tests the full join flow without WireGuard or TiKV:
//! 1. Node A initializes (mock backend)
//! 2. Node A starts peering listener (TLS)
//! 3. Node B sends join request
//! 4. Node B receives mesh secret, prefix, acceptor info
//! 5. Both nodes have consistent state

use syfrah_core::crypto::MeshSecret;
use syfrah_hypervisor::fabric::backend::NetworkMode;
use syfrah_hypervisor::fabric::mesh;
use syfrah_hypervisor::fabric::peer::PeerList;
use syfrah_hypervisor::fabric::peering::{JoinRequest, JoinResponse};
use syfrah_hypervisor::fabric::peering_client;
use syfrah_hypervisor::fabric::peering_server;
use syfrah_hypervisor::fabric::state::FabricState;
use syfrah_state::LocalDb;

fn temp_db() -> (tempfile::TempDir, LocalDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();
    (dir, db)
}

fn init_node(db: &LocalDb, name: &str) -> (FabricState, String) {
    let (mesh_id, secret) = mesh::create_mesh();
    let hv = mesh::create_hypervisor(name, "eu", "test", 51820, None, "", &mesh_id.prefix).unwrap();

    let secret_str = secret.to_string();
    let pin = secret.derive_pin();

    let state = FabricState {
        mesh: mesh_id,
        hypervisor: hv,
        secret: secret_str,
        peers: PeerList::new(),
        network_mode: NetworkMode::Mock,
    };
    state.save(db).unwrap();

    (state, pin)
}

#[tokio::test]
async fn peering_join_flow() {
    // Install crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // === Node A: init ===
    let (_dir_a, db_a) = temp_db();
    let (state_a, pin) = init_node(&db_a, "node-a");

    // === Node A: start peering server ===
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peering_addr = listener.local_addr().unwrap();

    let tls_config = syfrah_hypervisor::fabric::tls::server_config().unwrap();
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_config);

    let pin_clone = pin.clone();
    let db_a_clone = db_a.clone();
    let server = tokio::spawn(async move {
        let (tcp_stream, _peer_addr) = listener.accept().await.unwrap();
        let mut stream = tls_acceptor.accept(tcp_stream).await.unwrap();

        // Read join request
        let req: JoinRequest = peering_server::read_json(&mut stream).await.unwrap();
        assert_eq!(req.name, "node-b");
        assert_eq!(req.pin, Some(pin_clone.clone()));

        // Load state and build response
        let state = FabricState::load(&db_a_clone).unwrap().unwrap();

        let resp = JoinResponse::accepted(
            &state.secret,
            state.mesh.prefix,
            state.mesh.id.as_str(),
            vec![],
            syfrah_hypervisor::fabric::peering::PeerInfo {
                name: state.hypervisor.name.clone(),
                region: state.hypervisor.region.clone(),
                zone: state.hypervisor.zone.clone(),
                wg_public_key: state.hypervisor.wg_public_key.clone(),
                wg_port: state.hypervisor.wg_port,
                endpoint: None,
                mesh_ipv6: state.hypervisor.mesh_ipv6,
            },
        );
        peering_server::write_json(&mut stream, &resp).await.unwrap();
    });

    // === Node B: join ===
    let join_req = JoinRequest {
        name: "node-b".into(),
        region: "eu".into(),
        zone: "test".into(),
        wg_public_key: "dGVzdGtleQ==".into(), // base64 "testkey"
        wg_port: 51820,
        endpoint: None,
        pin: Some(pin.clone()),
    };

    let resp = peering_client::join(&peering_addr.to_string(), join_req).await.unwrap();

    // === Verify response ===
    assert!(resp.accepted);
    assert!(resp.secret.is_some());
    assert!(resp.prefix.is_some());
    assert!(resp.mesh_id.is_some());
    assert!(resp.acceptor.is_some());

    let acceptor = resp.acceptor.unwrap();
    assert_eq!(acceptor.name, "node-a");
    assert_eq!(acceptor.region, "eu");

    // Verify secret is valid
    let secret: MeshSecret = resp.secret.unwrap().parse().unwrap();
    assert!(secret.to_string().starts_with("syf_sk_"));

    // Verify mesh ID matches
    assert_eq!(resp.mesh_id.unwrap(), state_a.mesh.id.as_str());

    server.await.unwrap();
}

#[tokio::test]
async fn peering_wrong_pin_rejected() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (_dir_a, db_a) = temp_db();
    let (_state_a, _pin) = init_node(&db_a, "node-a");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let tls_config = syfrah_hypervisor::fabric::tls::server_config().unwrap();
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_config);

    let server = tokio::spawn(async move {
        let (tcp, _peer_addr) = listener.accept().await.unwrap();
        let mut stream = tls_acceptor.accept(tcp).await.unwrap();
        let _req: JoinRequest = peering_server::read_json(&mut stream).await.unwrap();

        // Wrong PIN → reject
        let resp = JoinResponse::rejected("invalid PIN");
        peering_server::write_json(&mut stream, &resp).await.unwrap();
    });

    let req = JoinRequest {
        name: "attacker".into(),
        region: "eu".into(),
        zone: "test".into(),
        wg_public_key: "YmFk".into(),
        wg_port: 51820,
        endpoint: None,
        pin: Some("0000".into()), // wrong PIN
    };

    let result = peering_client::join(&addr.to_string(), req).await;
    assert!(result.is_err());

    server.await.unwrap();
}

#[tokio::test]
async fn api_server_health() {
    use axum::body::Body;
    use http::Request;
    use syfrah_core::api::{ApiConfig, ApiServer};
    use syfrah_hypervisor::handlers;
    use tower::ServiceExt;

    let server = ApiServer::new(ApiConfig::default(), vec![handlers::registration()], vec![]);

    // Health endpoint
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = server.admin_router().clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    // List endpoint (empty — not initialized)
    let req = Request::builder()
        .uri("/admin/v1/hypervisor")
        .body(Body::empty())
        .unwrap();
    let resp = server.admin_router().clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[test]
fn mock_backend_full_lifecycle() {
    let backend = syfrah_hypervisor::fabric::backend::create_backend(NetworkMode::Mock);

    assert_eq!(backend.mode(), NetworkMode::Mock);
    assert!(backend.is_up());
    assert!(backend.is_active());

    backend.ensure_installed().unwrap();
    backend
        .setup("key", 51820, &"fd01::1".parse().unwrap(), &[])
        .unwrap();

    let status = backend.status().unwrap();
    assert!(status.interface_up);

    backend.start().unwrap();
    backend.stop().unwrap();
    backend.teardown().unwrap();
}

#[test]
fn state_persistence_roundtrip() {
    let (_dir, db) = temp_db();

    // Init
    let (state, _pin) = init_node(&db, "test-node");
    assert_eq!(state.hypervisor.name, "test-node");
    assert_eq!(state.network_mode, NetworkMode::Mock);

    // Load
    let loaded = FabricState::load(&db).unwrap().unwrap();
    assert_eq!(loaded.hypervisor.name, "test-node");
    assert_eq!(loaded.mesh.id.as_str(), state.mesh.id.as_str());
    assert_eq!(loaded.network_mode, NetworkMode::Mock);

    // Delete
    FabricState::delete(&db).unwrap();
    assert!(FabricState::load(&db).unwrap().is_none());
}

#[test]
fn mesh_identity_generation() {
    let (mesh, secret) = mesh::create_mesh();
    assert!(mesh.id.as_str().starts_with("mesh-"));
    assert!(secret.to_string().starts_with("syf_sk_"));

    let hv = mesh::create_hypervisor("node-1", "eu", "fsn1", 51820, None, "", &mesh.prefix).unwrap();
    assert_eq!(hv.name, "node-1");
    assert!(!hv.wg_private_key.is_empty());
    assert!(!hv.wg_public_key.is_empty());

    // Mesh IPv6 in ULA range
    let first = hv.mesh_ipv6.segments()[0];
    assert!((0xfd00..=0xfdff).contains(&first));
}
