use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::handler::LayerHandler;

// ---------------------------------------------------------------------------
// LayerRequest / LayerResponse — the envelope that travels over the socket
// ---------------------------------------------------------------------------

/// Top-level request envelope. Each variant wraps an opaque payload that the
/// corresponding [`LayerHandler`] knows how to deserialise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayerRequest {
    /// Request destined for the Fabric layer.
    Fabric(Vec<u8>),
    /// Request destined for the Compute layer.
    Compute(Vec<u8>),
    /// Request destined for the Org layer.
    Org(Vec<u8>),
    /// Request destined for the Hypervisor layer.
    Hypervisor(Vec<u8>),
    /// Request destined for the Storage layer.
    Storage(Vec<u8>),
}

/// Top-level response envelope returned to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayerResponse {
    /// Response originating from the Fabric layer.
    Fabric(Vec<u8>),
    /// Response originating from the Compute layer.
    Compute(Vec<u8>),
    /// Response originating from the Org layer.
    Org(Vec<u8>),
    /// Response originating from the Hypervisor layer.
    Hypervisor(Vec<u8>),
    /// Response originating from the Storage layer.
    Storage(Vec<u8>),
    /// The requested layer is not registered in the router.
    UnknownLayer(String),
}

// ---------------------------------------------------------------------------
// LayerRouter
// ---------------------------------------------------------------------------

/// Dispatches incoming [`LayerRequest`]s to the correct [`LayerHandler`].
pub struct LayerRouter {
    handlers: HashMap<String, Arc<dyn LayerHandler>>,
}

impl LayerRouter {
    /// Create an empty router.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for a named layer (e.g. `"fabric"`).
    pub fn register(&mut self, layer: impl Into<String>, handler: Arc<dyn LayerHandler>) {
        self.handlers.insert(layer.into(), handler);
    }

    /// Route a [`LayerRequest`] to the appropriate handler and return a
    /// [`LayerResponse`].
    ///
    /// `caller_uid` is forwarded to the layer handler for audit logging.
    pub async fn dispatch(&self, request: LayerRequest, caller_uid: Option<u32>) -> LayerResponse {
        match request {
            LayerRequest::Fabric(payload) => {
                if let Some(handler) = self.handlers.get("fabric") {
                    LayerResponse::Fabric(handler.handle(payload, caller_uid).await)
                } else {
                    LayerResponse::UnknownLayer("fabric".into())
                }
            }
            LayerRequest::Compute(payload) => {
                if let Some(handler) = self.handlers.get("compute") {
                    LayerResponse::Compute(handler.handle(payload, caller_uid).await)
                } else {
                    LayerResponse::UnknownLayer("compute".into())
                }
            }
            LayerRequest::Org(payload) => {
                if let Some(handler) = self.handlers.get("org") {
                    LayerResponse::Org(handler.handle(payload, caller_uid).await)
                } else {
                    LayerResponse::UnknownLayer("org".into())
                }
            }
            LayerRequest::Hypervisor(payload) => {
                if let Some(handler) = self.handlers.get("hypervisor") {
                    LayerResponse::Hypervisor(handler.handle(payload, caller_uid).await)
                } else {
                    LayerResponse::UnknownLayer("hypervisor".into())
                }
            }
            LayerRequest::Storage(payload) => {
                if let Some(handler) = self.handlers.get("storage") {
                    LayerResponse::Storage(handler.handle(payload, caller_uid).await)
                } else {
                    LayerResponse::UnknownLayer("storage".into())
                }
            }
        }
    }
}

impl Default for LayerRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::LayerHandler;

    struct UpperHandler;

    #[async_trait::async_trait]
    impl LayerHandler for UpperHandler {
        async fn handle(&self, request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
            request.iter().map(|b| b.to_ascii_uppercase()).collect()
        }
    }

    #[tokio::test]
    async fn dispatch_to_fabric() {
        let mut router = LayerRouter::new();
        router.register("fabric", Arc::new(UpperHandler));

        let req = LayerRequest::Fabric(b"hello".to_vec());
        let resp = router.dispatch(req, None).await;

        match resp {
            LayerResponse::Fabric(data) => assert_eq!(data, b"HELLO"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_layer() {
        let router = LayerRouter::new(); // no handlers registered

        let req = LayerRequest::Fabric(b"test".to_vec());
        let resp = router.dispatch(req, None).await;

        match resp {
            LayerResponse::UnknownLayer(name) => assert_eq!(name, "fabric"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn layer_request_serde_roundtrip() {
        let req = LayerRequest::Fabric(b"payload".to_vec());
        let json = serde_json::to_vec(&req).unwrap();
        let back: LayerRequest = serde_json::from_slice(&json).unwrap();
        match back {
            LayerRequest::Fabric(data) => assert_eq!(data, b"payload"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn layer_response_serde_roundtrip() {
        let resp = LayerResponse::Fabric(b"result".to_vec());
        let json = serde_json::to_vec(&resp).unwrap();
        let back: LayerResponse = serde_json::from_slice(&json).unwrap();
        match back {
            LayerResponse::Fabric(data) => assert_eq!(data, b"result"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn register_fabric_send_request_get_response() {
        let mut router = LayerRouter::new();
        router.register("fabric", Arc::new(UpperHandler));

        // Send a fabric request and verify we get the uppercased response.
        let req = LayerRequest::Fabric(b"syfrah".to_vec());
        let resp = router.dispatch(req, Some(1000)).await;

        match resp {
            LayerResponse::Fabric(data) => {
                assert_eq!(data, b"SYFRAH");
            }
            other => panic!("expected Fabric response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn replacing_handler_uses_latest() {
        struct ReverseHandler;

        #[async_trait::async_trait]
        impl LayerHandler for ReverseHandler {
            async fn handle(&self, request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
                request.into_iter().rev().collect()
            }
        }

        let mut router = LayerRouter::new();
        // Register first handler (uppercase).
        router.register("fabric", Arc::new(UpperHandler));
        // Replace with second handler (reverse).
        router.register("fabric", Arc::new(ReverseHandler));

        let req = LayerRequest::Fabric(b"abc".to_vec());
        let resp = router.dispatch(req, None).await;

        match resp {
            LayerResponse::Fabric(data) => {
                assert_eq!(data, b"cba", "should use the latest registered handler");
            }
            other => panic!("expected Fabric response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn caller_uid_forwarded_to_handler() {
        struct UidEchoHandler;

        #[async_trait::async_trait]
        impl LayerHandler for UidEchoHandler {
            async fn handle(&self, _request: Vec<u8>, caller_uid: Option<u32>) -> Vec<u8> {
                match caller_uid {
                    Some(uid) => uid.to_be_bytes().to_vec(),
                    None => vec![],
                }
            }
        }

        let mut router = LayerRouter::new();
        router.register("fabric", Arc::new(UidEchoHandler));

        let req = LayerRequest::Fabric(vec![]);
        let resp = router.dispatch(req, Some(42)).await;

        match resp {
            LayerResponse::Fabric(data) => {
                let uid = u32::from_be_bytes(data.try_into().unwrap());
                assert_eq!(uid, 42);
            }
            other => panic!("expected Fabric response, got: {other:?}"),
        }
    }
}
