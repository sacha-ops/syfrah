#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use crate::backend::NetworkBackend;
    use crate::mock::{MockBackend, MockCall};

    #[tokio::test]
    async fn create_bridge() {
        let backend = MockBackend::new();
        backend.create_bridge("syfbr-100").await.unwrap();

        assert_eq!(
            backend.calls(),
            vec![MockCall::CreateBridge {
                name: "syfbr-100".into()
            }]
        );
    }

    #[tokio::test]
    async fn add_gateway_ip() {
        let backend = MockBackend::new();
        backend
            .add_bridge_ip("syfbr-100", Ipv4Addr::new(10, 1, 1, 1), 24)
            .await
            .unwrap();

        assert_eq!(
            backend.calls(),
            vec![MockCall::AddBridgeIp {
                bridge: "syfbr-100".into(),
                gateway: Ipv4Addr::new(10, 1, 1, 1),
                prefix_len: 24,
            }]
        );
    }

    #[tokio::test]
    async fn remove_gateway_ip() {
        let backend = MockBackend::new();
        backend
            .remove_bridge_ip("syfbr-100", Ipv4Addr::new(10, 1, 1, 1))
            .await
            .unwrap();

        assert_eq!(
            backend.calls(),
            vec![MockCall::RemoveBridgeIp {
                bridge: "syfbr-100".into(),
                gateway: Ipv4Addr::new(10, 1, 1, 1),
            }]
        );
    }

    #[tokio::test]
    async fn delete_bridge() {
        let backend = MockBackend::new();
        backend.delete_bridge("syfbr-100").await.unwrap();

        assert_eq!(
            backend.calls(),
            vec![MockCall::DeleteBridge {
                name: "syfbr-100".into()
            }]
        );
    }

    #[tokio::test]
    async fn idempotent_create() {
        let backend = MockBackend::new();
        backend.create_bridge("syfbr-200").await.unwrap();
        backend.create_bridge("syfbr-200").await.unwrap();

        // MockBackend records both calls — the real LinuxBackend would
        // skip the second via interface_exists check.  What matters is
        // that neither call returns an error.
        assert_eq!(backend.calls().len(), 2);
        assert!(backend
            .calls()
            .iter()
            .all(|c| matches!(c, MockCall::CreateBridge { .. })));
    }

    #[tokio::test]
    async fn multi_subnet_gateways() {
        let backend = MockBackend::new();
        let bridge = "syfbr-100";

        backend
            .add_bridge_ip(bridge, Ipv4Addr::new(10, 1, 1, 1), 24)
            .await
            .unwrap();
        backend
            .add_bridge_ip(bridge, Ipv4Addr::new(10, 1, 2, 1), 24)
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            MockCall::AddBridgeIp {
                bridge: bridge.into(),
                gateway: Ipv4Addr::new(10, 1, 1, 1),
                prefix_len: 24,
            }
        );
        assert_eq!(
            calls[1],
            MockCall::AddBridgeIp {
                bridge: bridge.into(),
                gateway: Ipv4Addr::new(10, 1, 2, 1),
                prefix_len: 24,
            }
        );
    }
}
