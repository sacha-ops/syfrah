//! Peering TCP+TLS client — sends join request to an existing node.

use std::net::SocketAddr;

use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use syfrah_core::error::SyfrahError;

use super::peering::{JoinRequest, JoinResponse, DEFAULT_PEERING_PORT};
use super::peering_server::{read_json, write_json};

/// Send a join request to a target node over TLS and return the response.
pub async fn join(target: &str, request: JoinRequest) -> Result<JoinResponse, SyfrahError> {
    // Parse target address
    let addr: SocketAddr = if target.contains(':') {
        target
            .parse()
            .map_err(|_| SyfrahError::validation(format!("invalid target address: {target}")))?
    } else {
        format!("{target}:{DEFAULT_PEERING_PORT}")
            .parse()
            .map_err(|_| SyfrahError::validation(format!("invalid target address: {target}")))?
    };

    // TCP connect
    let tcp_stream = TcpStream::connect(addr)
        .await
        .map_err(|e| SyfrahError::network(format!("failed to connect to {addr}: {e}")))?;

    // TLS handshake (TOFU — accept any cert, PIN provides auth)
    let tls_config = super::tls::client_config();
    let connector = TlsConnector::from(tls_config);
    let server_name = rustls::pki_types::ServerName::try_from("syfrah-peering")
        .map_err(|e| SyfrahError::internal(format!("TLS server name: {e}")))?;

    let mut stream = connector
        .connect(server_name, tcp_stream)
        .await
        .map_err(|e| SyfrahError::network(format!("TLS handshake failed: {e}")))?;

    // Send request
    write_json(&mut stream, &request).await?;

    // Read response
    let response: JoinResponse = read_json(&mut stream).await?;

    if !response.accepted {
        let reason = response.reason.unwrap_or_else(|| "unknown".to_string());
        return Err(SyfrahError::permission_denied(format!(
            "join rejected: {reason}"
        )));
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn join_connection_refused() {
        let result = join(
            "127.0.0.1:19999",
            JoinRequest {
                name: "test".into(),
                region: "eu".into(),
                zone: "fsn1".into(),
                wg_public_key: "k".into(),
                wg_port: 51820,
                endpoint: None,
                pin: None,
            },
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn join_tls_roundtrip() {
        use tokio::net::TcpListener;
        use tokio_rustls::TlsAcceptor;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let tls_config = super::super::tls::server_config().unwrap();
        let tls_acceptor = TlsAcceptor::from(tls_config);

        let server = tokio::spawn(async move {
            let (tcp_stream, _) = listener.accept().await.unwrap();
            let mut stream = tls_acceptor.accept(tcp_stream).await.unwrap();
            let _req: JoinRequest = read_json(&mut stream).await.unwrap();
            let resp = JoinResponse::accepted(
                "syf_sk_test",
                "fd01::".parse().unwrap(),
                "mesh-test",
                vec![],
                super::super::peering::PeerInfo {
                    name: "init-node".into(),
                    region: "eu".into(),
                    zone: "fsn1".into(),
                    wg_public_key: "initkey".into(),
                    wg_port: 51820,
                    endpoint: Some("1.2.3.4:51820".into()),
                    mesh_ipv6: "fd01::1".parse().unwrap(),
                },
            );
            write_json(&mut stream, &resp).await.unwrap();
        });

        let req = JoinRequest {
            name: "joiner".into(),
            region: "eu".into(),
            zone: "nbg1".into(),
            wg_public_key: "joinerkey".into(),
            wg_port: 51820,
            endpoint: None,
            pin: Some("1234".into()),
        };

        let resp = join(&addr.to_string(), req).await.unwrap();
        assert!(resp.accepted);
        assert!(resp.secret.is_some());

        server.await.unwrap();
    }
}
