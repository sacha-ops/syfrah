//! Control socket transport protocol.
//!
//! The protocol between CLI and daemon uses Unix domain sockets with
//! length-prefixed JSON messages. This module provides:
//!
//! - **Message framing**: 4-byte big-endian length prefix + JSON payload
//! - **Request/Response types**: generic, resource-kind-based routing
//! - **Server**: bind, accept, dispatch
//! - **Client**: connect, send request, read response
//!
//! # Protocol
//!
//! ```text
//! CLI                              Daemon
//!  │                                 │
//!  │──── [4 bytes len][JSON req] ───→│
//!  │                                 │── dispatch to handler
//!  │←── [4 bytes len][JSON resp] ────│
//!  │                                 │
//!  └── close ────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use syfrah_core::transport::{Request, send_request};
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let req = Request::resource("vpc", "create", Some("my-vpc".into()), Default::default());
//! let resp = send_request(Path::new("/root/.syfrah/control.sock"), &req).await?;
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::error::SyfrahError;

/// Maximum message size: 1 MB.
pub const MAX_MESSAGE_SIZE: u32 = 1_048_576;

/// Default socket path.
pub const DEFAULT_SOCKET_PATH: &str = ".syfrah/control.sock";

/// Get the default control socket path for the current user.
pub fn socket_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/root"))
        .join(DEFAULT_SOCKET_PATH)
}

// ═══════════════════════════════════════════════════
// Request / Response — the wire protocol
// ═══════════════════════════════════════════════════

/// A request from CLI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Resource kind: "fabric", "hypervisor", "vpc", etc.
    pub kind: String,
    /// Operation: "create", "list", "get", "delete", or custom action
    pub operation: String,
    /// Resource name or ID (for get/delete/create)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Scope values (--org, --vpc, etc.)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub scope: HashMap<String, String>,
    /// Field values from CLI flags
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fields: HashMap<String, String>,
}

/// A response from daemon to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Whether the operation succeeded.
    pub ok: bool,
    /// Response payload (resource JSON, list, message).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error details (if ok=false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SyfrahError>,
}

impl Request {
    /// Create a resource operation request.
    pub fn resource(
        kind: impl Into<String>,
        operation: impl Into<String>,
        name: Option<String>,
        fields: HashMap<String, String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            operation: operation.into(),
            name,
            scope: HashMap::new(),
            fields,
        }
    }

    /// Add a scope value.
    pub fn with_scope(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.scope.insert(key.into(), value.into());
        self
    }
}

impl Response {
    /// Success with data.
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// Success with no data (e.g., delete).
    pub fn ok_empty() -> Self {
        Self {
            ok: true,
            data: None,
            error: None,
        }
    }

    /// Success with a message.
    pub fn ok_message(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            data: Some(serde_json::Value::String(msg.into())),
            error: None,
        }
    }

    /// Error response.
    pub fn err(error: SyfrahError) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(error),
        }
    }
}

// ═══════════════════════════════════════════════════
// Wire framing — length-prefixed JSON
// ═══════════════════════════════════════════════════

/// Write a length-prefixed JSON message.
pub async fn write_message<T: Serialize, W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    msg: &T,
) -> Result<(), SyfrahError> {
    let data = serde_json::to_vec(msg).map_err(SyfrahError::from)?;
    let len = data.len() as u32;
    if len > MAX_MESSAGE_SIZE {
        return Err(SyfrahError::validation(format!(
            "message too large: {len} bytes (max {MAX_MESSAGE_SIZE})"
        )));
    }
    stream
        .write_all(&len.to_be_bytes())
        .await
        .map_err(SyfrahError::from)?;
    stream.write_all(&data).await.map_err(SyfrahError::from)?;
    stream.flush().await.map_err(SyfrahError::from)?;
    Ok(())
}

/// Read a length-prefixed JSON message.
pub async fn read_message<T: serde::de::DeserializeOwned, R: AsyncReadExt + Unpin>(
    stream: &mut R,
) -> Result<T, SyfrahError> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(SyfrahError::from)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(SyfrahError::validation(format!(
            "message too large: {len} bytes (max {MAX_MESSAGE_SIZE})"
        )));
    }
    let mut data = vec![0u8; len as usize];
    stream
        .read_exact(&mut data)
        .await
        .map_err(SyfrahError::from)?;
    serde_json::from_slice(&data).map_err(SyfrahError::from)
}

// ═══════════════════════════════════════════════════
// Client — CLI side
// ═══════════════════════════════════════════════════

/// Send a request to the daemon and return the response.
pub async fn send_request(socket: &Path, req: &Request) -> Result<Response, SyfrahError> {
    let mut stream = UnixStream::connect(socket).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound
            || e.kind() == std::io::ErrorKind::ConnectionRefused
        {
            SyfrahError::daemon_unreachable()
        } else {
            SyfrahError::from(e)
        }
    })?;

    write_message(&mut stream, req).await?;

    // Shutdown write half to signal end of request
    stream.shutdown().await.map_err(SyfrahError::from)?;

    read_message(&mut stream).await
}

// ═══════════════════════════════════════════════════
// Server — daemon side
// ═══════════════════════════════════════════════════

/// Bind a Unix listener with restrictive permissions (0o600).
pub fn bind_listener(socket_path: &Path) -> Result<tokio::net::UnixListener, SyfrahError> {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);

    // Set restrictive umask before bind
    #[cfg(unix)]
    let old_umask = unsafe { libc::umask(0o177) };

    let listener = tokio::net::UnixListener::bind(socket_path).map_err(|e| {
        #[cfg(unix)]
        unsafe {
            libc::umask(old_umask);
        }
        SyfrahError::from(e)
    })?;

    #[cfg(unix)]
    unsafe {
        libc::umask(old_umask);
    }

    Ok(listener)
}

/// Handler trait — daemon registers one per resource kind.
#[async_trait::async_trait]
pub trait RequestHandler: Send + Sync {
    /// Handle a request and return a response.
    async fn handle(&self, req: Request, caller_uid: Option<u32>) -> Response;
}

/// Router — dispatches requests to the correct handler by resource kind.
pub struct Router {
    handlers: HashMap<String, Box<dyn RequestHandler>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for a resource kind.
    pub fn register(&mut self, kind: impl Into<String>, handler: impl RequestHandler + 'static) {
        self.handlers.insert(kind.into(), Box::new(handler));
    }

    /// Dispatch a request to the appropriate handler.
    pub async fn dispatch(&self, req: Request, caller_uid: Option<u32>) -> Response {
        match self.handlers.get(&req.kind) {
            Some(handler) => handler.handle(req, caller_uid).await,
            None => Response::err(SyfrahError::not_found("layer", &req.kind).with_suggestion(
                format!(
                    "Available layers: {}",
                    self.handlers.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            )),
        }
    }

    /// List registered handler kinds.
    pub fn kinds(&self) -> Vec<&str> {
        self.handlers.keys().map(|k| k.as_str()).collect()
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    // ── Wire framing tests ──

    #[tokio::test]
    async fn message_roundtrip() {
        let (mut client, mut server) = duplex(4096);

        let req = Request::resource("vpc", "create", Some("my-vpc".into()), HashMap::new());
        write_message(&mut client, &req).await.unwrap();
        drop(client);

        let read_req: Request = read_message(&mut server).await.unwrap();
        assert_eq!(read_req.kind, "vpc");
        assert_eq!(read_req.operation, "create");
        assert_eq!(read_req.name, Some("my-vpc".into()));
    }

    #[tokio::test]
    async fn response_roundtrip() {
        let (mut client, mut server) = duplex(4096);

        let resp = Response::ok(serde_json::json!({"name": "my-vpc", "cidr": "10.0.0.0/16"}));
        write_message(&mut client, &resp).await.unwrap();
        drop(client);

        let read_resp: Response = read_message(&mut server).await.unwrap();
        assert!(read_resp.ok);
        assert!(read_resp.data.is_some());
    }

    #[tokio::test]
    async fn error_response_roundtrip() {
        let (mut client, mut server) = duplex(4096);

        let resp = Response::err(SyfrahError::not_found("vpc", "web"));
        write_message(&mut client, &resp).await.unwrap();
        drop(client);

        let read_resp: Response = read_message(&mut server).await.unwrap();
        assert!(!read_resp.ok);
        assert!(read_resp.error.is_some());
        assert!(read_resp.error.unwrap().message.contains("web"));
    }

    #[tokio::test]
    async fn oversized_message_rejected() {
        let (mut client, mut server) = duplex(64);

        let fake_len: u32 = MAX_MESSAGE_SIZE + 1;
        tokio::io::AsyncWriteExt::write_all(&mut client, &fake_len.to_be_bytes())
            .await
            .unwrap();
        drop(client);

        let result: Result<Request, _> = read_message(&mut server).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn malformed_json_rejected() {
        let (mut client, mut server) = duplex(4096);

        let bad = b"not json at all";
        let len = bad.len() as u32;
        tokio::io::AsyncWriteExt::write_all(&mut client, &len.to_be_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut client, bad)
            .await
            .unwrap();
        drop(client);

        let result: Result<Request, _> = read_message(&mut server).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_stream_errors() {
        let (_client, mut server) = duplex(4096);
        drop(_client);

        let result: Result<Request, _> = read_message(&mut server).await;
        assert!(result.is_err());
    }

    // ── Request/Response construction ──

    #[test]
    fn request_with_scope() {
        let req = Request::resource("subnet", "create", Some("web".into()), HashMap::new())
            .with_scope("org", "acme")
            .with_scope("vpc", "my-vpc");
        assert_eq!(req.scope.get("org"), Some(&"acme".to_string()));
        assert_eq!(req.scope.get("vpc"), Some(&"my-vpc".to_string()));
    }

    #[test]
    fn response_ok_message() {
        let resp = Response::ok_message("vpc 'my-vpc' created.");
        assert!(resp.ok);
        assert_eq!(
            resp.data.unwrap().as_str().unwrap(),
            "vpc 'my-vpc' created."
        );
    }

    #[test]
    fn response_ok_empty() {
        let resp = Response::ok_empty();
        assert!(resp.ok);
        assert!(resp.data.is_none());
    }

    #[test]
    fn response_serde_skips_none() {
        let resp = Response::ok_empty();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("data"));
        assert!(!json.contains("error"));
    }

    // ── Router tests ──

    struct EchoHandler;

    #[async_trait::async_trait]
    impl RequestHandler for EchoHandler {
        async fn handle(&self, req: Request, _caller_uid: Option<u32>) -> Response {
            Response::ok(serde_json::json!({
                "echoed_kind": req.kind,
                "echoed_op": req.operation,
            }))
        }
    }

    #[tokio::test]
    async fn router_dispatches_to_handler() {
        let mut router = Router::new();
        router.register("vpc", EchoHandler);

        let req = Request::resource("vpc", "list", None, HashMap::new());
        let resp = router.dispatch(req, None).await;

        assert!(resp.ok);
        let data = resp.data.unwrap();
        assert_eq!(data["echoed_kind"], "vpc");
        assert_eq!(data["echoed_op"], "list");
    }

    #[tokio::test]
    async fn router_unknown_kind() {
        let router = Router::new();

        let req = Request::resource("nonexistent", "list", None, HashMap::new());
        let resp = router.dispatch(req, None).await;

        assert!(!resp.ok);
        assert!(resp.error.is_some());
    }

    #[test]
    fn router_kinds() {
        let mut router = Router::new();
        router.register("fabric", EchoHandler);
        router.register("vpc", EchoHandler);

        let mut kinds = router.kinds();
        kinds.sort();
        assert_eq!(kinds, vec!["fabric", "vpc"]);
    }

    // ── Socket path ──

    #[test]
    fn socket_path_is_in_syfrah_dir() {
        let path = socket_path();
        assert!(path.to_str().unwrap().contains(".syfrah/control.sock"));
    }
}
