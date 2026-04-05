//! Peering TCP client — sends join request to an existing node.

use std::net::SocketAddr;

use tokio::net::TcpStream;

use syfrah_core::error::SyfrahError;

use super::peering::{JoinRequest, JoinResponse, DEFAULT_PEERING_PORT};
use super::peering_server::{read_json, write_json};

/// Send a join request to a target node and return the response.
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

    // Connect
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| SyfrahError::network(format!("failed to connect to {addr}: {e}")))?;

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
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn join_rejected() {
        // Start a fake server that rejects
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _req: JoinRequest = read_json(&mut stream).await.unwrap();
            let resp = JoinResponse::rejected("bad pin");
            write_json(&mut stream, &resp).await.unwrap();
        });

        let req = JoinRequest {
            name: "joiner".into(),
            region: "eu".into(),
            zone: "nbg1".into(),
            wg_public_key: "key".into(),
            wg_port: 51820,
            endpoint: None,
            pin: Some("wrong".into()),
        };

        let result = join(&addr.to_string(), req).await;
        assert!(result.is_err());

        server.await.unwrap();
    }

    #[tokio::test]
    async fn join_accepted() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _req: JoinRequest = read_json(&mut stream).await.unwrap();
            let resp = JoinResponse::accepted(
                "syf_sk_test",
                "fd01::".parse().unwrap(),
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
        assert!(resp.acceptor.is_some());

        server.await.unwrap();
    }

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
}
