#[cfg(test)]
mod tests {
    use crate::backend::NetworkBackend;
    use crate::mock::MockBackend;

    #[tokio::test]
    async fn create_bridge() {
        let backend = MockBackend::new();
        backend.create_bridge("syfbr-100").await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "create_bridge(syfbr-100)");
    }

    #[tokio::test]
    async fn add_gateway_ip() {
        let backend = MockBackend::new();
        backend
            .add_bridge_ip("syfbr-100", "10.1.1.1", 24)
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "add_bridge_ip(syfbr-100, 10.1.1.1, 24)");
    }

    #[tokio::test]
    async fn remove_gateway_ip() {
        let backend = MockBackend::new();
        backend
            .remove_bridge_ip("syfbr-100", "10.1.1.1")
            .await
            .unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "remove_bridge_ip(syfbr-100, 10.1.1.1)");
    }

    #[tokio::test]
    async fn delete_bridge() {
        let backend = MockBackend::new();
        backend.delete_bridge("syfbr-100").await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "delete_bridge(syfbr-100)");
    }

    #[tokio::test]
    async fn idempotent_create() {
        let backend = MockBackend::new();
        backend.create_bridge("syfbr-200").await.unwrap();
        backend.create_bridge("syfbr-200").await.unwrap();

        // MockBackend records both calls — the real LinuxBackend would
        // skip the second via interface_exists check.  What matters is
        // that neither call returns an error.
        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|c| c.starts_with("create_bridge(")));
    }

    #[tokio::test]
    async fn multi_subnet_gateways() {
        let backend = MockBackend::new();
        let bridge = "syfbr-100";

        backend.add_bridge_ip(bridge, "10.1.1.1", 24).await.unwrap();
        backend.add_bridge_ip(bridge, "10.1.2.1", 24).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], "add_bridge_ip(syfbr-100, 10.1.1.1, 24)");
        assert_eq!(calls[1], "add_bridge_ip(syfbr-100, 10.1.2.1, 24)");
    }
}
