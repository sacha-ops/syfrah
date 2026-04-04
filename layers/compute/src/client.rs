use std::path::PathBuf;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::client::conn;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tracing::debug;

use crate::error::ClientError;

/// HTTP client for the Cloud Hypervisor REST API over a Unix socket.
///
/// Each VM exposes its own socket at `/run/syfrah/vms/{id}/api.sock`.
/// `ChClient` wraps one socket path and provides typed methods for every
/// CH endpoint that Syfrah uses.
pub struct ChClient {
    socket_path: PathBuf,
    timeout: Duration,
}

/// Default request timeout (10 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

impl ChClient {
    /// Create a new client for the given socket path with the default 10s timeout.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Create a new client with a custom timeout.
    pub fn with_timeout(socket_path: PathBuf, timeout: Duration) -> Self {
        Self {
            socket_path,
            timeout,
        }
    }

    /// Send an HTTP request over the Unix socket.
    ///
    /// Returns `Some(json)` for 200 responses with a body, `None` for 204 (no content).
    /// Non-2xx responses are returned as errors unless the caller handles them
    /// via idempotence logic.
    async fn request(
        &self,
        method: hyper::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, Option<serde_json::Value>), ClientError> {
        // Check socket exists before attempting connection.
        if !self.socket_path.exists() {
            return Err(ClientError::SocketNotFound {
                path: self.socket_path.display().to_string(),
            });
        }

        let result = tokio::time::timeout(self.timeout, self.do_request(method, path, body)).await;

        match result {
            Ok(inner) => inner,
            Err(_) => Err(ClientError::Timeout {
                operation: path.to_string(),
            }),
        }
    }

    /// Perform the actual HTTP request (no timeout wrapper).
    async fn do_request(
        &self,
        method: hyper::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, Option<serde_json::Value>), ClientError> {
        let stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::ConnectionRefused {
                ClientError::ConnectionRefused
            } else if e.kind() == std::io::ErrorKind::NotFound {
                ClientError::SocketNotFound {
                    path: self.socket_path.display().to_string(),
                }
            } else {
                ClientError::ConnectionRefused
            }
        })?;

        let io = TokioIo::new(stream);
        let (mut sender, connection) =
            conn::http1::handshake(io)
                .await
                .map_err(|_| ClientError::InvalidResponse {
                    detail: "HTTP handshake failed".to_string(),
                })?;

        // Spawn the connection driver so it processes in the background.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                debug!("connection driver error: {e}");
            }
        });

        let req_body = match &body {
            Some(json) => Full::new(Bytes::from(
                serde_json::to_vec(json).expect("JSON serialization cannot fail"),
            )),
            None => Full::new(Bytes::new()),
        };

        let mut builder = Request::builder()
            .method(method)
            .uri(format!("http://localhost/api/v1{path}"))
            .header("Host", "localhost");

        if body.is_some() {
            builder = builder.header("Content-Type", "application/json");
        }

        let req = builder
            .body(req_body)
            .map_err(|_| ClientError::InvalidResponse {
                detail: "failed to build request".to_string(),
            })?;

        let response =
            sender
                .send_request(req)
                .await
                .map_err(|_| ClientError::InvalidResponse {
                    detail: "request failed".to_string(),
                })?;

        let status = response.status().as_u16();

        // Collect the response body.
        let body_bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| ClientError::InvalidResponse {
                detail: "failed to read response body".to_string(),
            })?
            .to_bytes();

        if status == 204 || body_bytes.is_empty() {
            return Ok((status, None));
        }

        let json: serde_json::Value =
            serde_json::from_slice(&body_bytes).map_err(|e| ClientError::InvalidResponse {
                detail: format!("invalid JSON in response: {e}"),
            })?;

        Ok((status, Some(json)))
    }

    // ── GA endpoints (#468) ──────────────────────────────────────────

    /// Health check. Returns `true` if the VMM is responding (HTTP 200).
    pub async fn ping(&self) -> Result<bool, ClientError> {
        let (status, _) = self.request(hyper::Method::GET, "/vmm.ping", None).await?;
        Ok(status == 200)
    }

    /// VM info. Returns the full VM status JSON from Cloud Hypervisor.
    pub async fn info(&self) -> Result<serde_json::Value, ClientError> {
        let (status, body) = self.request(hyper::Method::GET, "/vm.info", None).await?;
        if !(200..300).contains(&status) {
            return Err(ClientError::UnexpectedStatus {
                status,
                body: body.map(|v| v.to_string()).unwrap_or_default(),
            });
        }
        body.ok_or(ClientError::InvalidResponse {
            detail: "expected JSON body from /vm.info".to_string(),
        })
    }

    /// Create a VM definition. NOT idempotent — creating twice is a bug (409 = error).
    pub async fn create(&self, config: serde_json::Value) -> Result<(), ClientError> {
        let (status, body) = self
            .request(hyper::Method::PUT, "/vm.create", Some(config))
            .await?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        // create is explicitly NOT idempotent: 409 means the VM already exists,
        // and the caller has a bug.
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Boot the VM. Idempotent: if already booted (409), returns Ok.
    pub async fn boot(&self) -> Result<(), ClientError> {
        let (status, body) = self.request(hyper::Method::PUT, "/vm.boot", None).await?;
        if (200..300).contains(&status) || is_idempotent_ok(status, "boot") {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// ACPI shutdown signal to guest. Idempotent: if already stopped (404), returns Ok.
    pub async fn shutdown_graceful(&self) -> Result<(), ClientError> {
        let (status, body) = self
            .request(hyper::Method::PUT, "/vm.shutdown", None)
            .await?;
        if (200..300).contains(&status) || is_idempotent_ok(status, "shutdown_graceful") {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Power button (guest-level force). Idempotent: if already stopped (404), returns Ok.
    pub async fn shutdown_force(&self) -> Result<(), ClientError> {
        let (status, body) = self
            .request(hyper::Method::PUT, "/vm.power-button", None)
            .await?;
        if (200..300).contains(&status) || is_idempotent_ok(status, "shutdown_force") {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Delete VM definition. Idempotent: if already deleted (404), returns Ok.
    pub async fn delete(&self) -> Result<(), ClientError> {
        let (status, body) = self.request(hyper::Method::PUT, "/vm.delete", None).await?;
        if (200..300).contains(&status) || is_idempotent_ok(status, "delete") {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    // ── Beta endpoints (#469) ────────────────────────────────────────

    /// Reboot guest. NOT idempotent on stopped VM — must be running.
    pub async fn reboot(&self) -> Result<(), ClientError> {
        let (status, body) = self.request(hyper::Method::PUT, "/vm.reboot", None).await?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Pause VM execution. Idempotent: if already paused (409), returns Ok.
    pub async fn pause(&self) -> Result<(), ClientError> {
        let (status, body) = self.request(hyper::Method::PUT, "/vm.pause", None).await?;
        if (200..300).contains(&status) || is_idempotent_ok(status, "pause") {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Resume paused VM. Idempotent: if already running (409), returns Ok.
    pub async fn resume(&self) -> Result<(), ClientError> {
        let (status, body) = self.request(hyper::Method::PUT, "/vm.resume", None).await?;
        if (200..300).contains(&status) || is_idempotent_ok(status, "resume") {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Hot-resize CPU and/or memory. NOT idempotent on stopped VM — must be running.
    pub async fn resize(
        &self,
        vcpus: Option<u32>,
        memory_bytes: Option<u64>,
    ) -> Result<(), ClientError> {
        let mut body = serde_json::Map::new();
        if let Some(v) = vcpus {
            body.insert("desired_vcpus".to_string(), serde_json::Value::from(v));
        }
        if let Some(m) = memory_bytes {
            body.insert("desired_ram".to_string(), serde_json::Value::from(m));
        }

        let (status, resp_body) = self
            .request(
                hyper::Method::PUT,
                "/vm.resize",
                Some(serde_json::Value::Object(body)),
            )
            .await?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: resp_body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    // ── Hot-plug disk endpoints (#1199) ────────────────────────────────

    /// Hot-add a disk to a running VM.
    ///
    /// `disk_config` must match Cloud Hypervisor's `DiskConfig` schema (at minimum
    /// `path` and optionally `readonly`, `id`). Returns the device ID assigned by
    /// CH (from the `id` field in the response, or the one supplied in the request).
    ///
    /// NOT idempotent — adding the same disk twice is an error (409).
    pub async fn add_disk(
        &self,
        disk_config: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, ClientError> {
        let (status, body) = self
            .request(hyper::Method::PUT, "/vm.add-disk", Some(disk_config))
            .await?;
        if (200..300).contains(&status) {
            return Ok(body);
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Hot-remove a device from a running VM by its Cloud Hypervisor device ID.
    ///
    /// Sends `{"id": device_id}` to `PUT /vm.remove-device`.
    /// Idempotent: if the device is already removed (404), returns Ok.
    pub async fn remove_device(&self, device_id: &str) -> Result<(), ClientError> {
        let body = serde_json::json!({ "id": device_id });
        let (status, resp_body) = self
            .request(hyper::Method::PUT, "/vm.remove-device", Some(body))
            .await?;
        if (200..300).contains(&status) || status == 404 {
            return Ok(());
        }
        Err(ClientError::UnexpectedStatus {
            status,
            body: resp_body.map(|v| v.to_string()).unwrap_or_default(),
        })
    }

    /// Performance counters. Returns parsed JSON.
    pub async fn counters(&self) -> Result<serde_json::Value, ClientError> {
        let (status, body) = self
            .request(hyper::Method::GET, "/vm.counters", None)
            .await?;
        if !(200..300).contains(&status) {
            return Err(ClientError::UnexpectedStatus {
                status,
                body: body.map(|v| v.to_string()).unwrap_or_default(),
            });
        }
        body.ok_or(ClientError::InvalidResponse {
            detail: "expected JSON body from /vm.counters".to_string(),
        })
    }
}

// ── Idempotence logic (#471) ─────────────────────────────────────────

/// Determine whether a non-2xx HTTP status should be treated as a
/// successful no-op for the given operation.
///
/// Cloud Hypervisor returns specific status codes when an operation is
/// applied to a VM that is already in the target state. For reconciliation
/// loops, these are success — the desired state already matches reality.
///
/// | Operation          | Idempotent status | Meaning                |
/// |--------------------|-------------------|------------------------|
/// | boot               | 409               | Already booted         |
/// | shutdown_graceful   | 404               | Already stopped        |
/// | shutdown_force      | 404               | Already stopped        |
/// | delete             | 404               | Already deleted/absent |
/// | pause              | 409               | Already paused         |
/// | resume             | 409               | Already running        |
fn is_idempotent_ok(status: u16, operation: &str) -> bool {
    match operation {
        // Already booted → no-op.
        "boot" => status == 409,
        // Already stopped → no-op.
        "shutdown_graceful" | "shutdown_force" => status == 404,
        // Already deleted → no-op.
        "delete" => status == 404,
        // Already paused → no-op.
        "pause" => status == 409,
        // Already running → no-op.
        "resume" => status == 409,
        // All other operations: non-2xx is an error.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{MockChServer, MockResponse};
    use tempfile::TempDir;

    /// Helper: create a temp dir and return (dir, socket_path).
    fn temp_socket() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let sock = dir.path().join("api.sock");
        (dir, sock)
    }

    // ── Idempotence truth table ──────────────────────────────────────

    #[test]
    fn idempotent_boot_409() {
        assert!(is_idempotent_ok(409, "boot"));
    }

    #[test]
    fn idempotent_boot_404_is_not_ok() {
        assert!(!is_idempotent_ok(404, "boot"));
    }

    #[test]
    fn idempotent_shutdown_graceful_404() {
        assert!(is_idempotent_ok(404, "shutdown_graceful"));
    }

    #[test]
    fn idempotent_shutdown_force_404() {
        assert!(is_idempotent_ok(404, "shutdown_force"));
    }

    #[test]
    fn idempotent_delete_404() {
        assert!(is_idempotent_ok(404, "delete"));
    }

    #[test]
    fn idempotent_pause_409() {
        assert!(is_idempotent_ok(409, "pause"));
    }

    #[test]
    fn idempotent_resume_409() {
        assert!(is_idempotent_ok(409, "resume"));
    }

    #[test]
    fn create_is_not_idempotent() {
        assert!(!is_idempotent_ok(409, "create"));
    }

    #[test]
    fn reboot_is_not_idempotent() {
        assert!(!is_idempotent_ok(409, "reboot"));
    }

    #[test]
    fn resize_is_not_idempotent() {
        assert!(!is_idempotent_ok(409, "resize"));
    }

    // ── Constructor tests ────────────────────────────────────────────

    #[test]
    fn new_uses_default_timeout() {
        let client = ChClient::new(PathBuf::from("/tmp/test.sock"));
        assert_eq!(client.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn with_timeout_uses_custom() {
        let t = Duration::from_secs(30);
        let client = ChClient::with_timeout(PathBuf::from("/tmp/test.sock"), t);
        assert_eq!(client.timeout, t);
    }

    // ── Socket-not-found test ────────────────────────────────────────

    #[tokio::test]
    async fn request_to_missing_socket_returns_socket_not_found() {
        let client = ChClient::new(PathBuf::from("/tmp/nonexistent-ch-test.sock"));
        let result = client.ping().await;
        assert!(matches!(result, Err(ClientError::SocketNotFound { .. })));
    }

    // ── ping tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn ping_returns_true_on_200() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "GET",
            "/vmm.ping",
            MockResponse::ok_json(r#"{"pong":true}"#),
        );
        mock.start().await;

        let client = ChClient::new(sock);
        let result = client.ping().await.expect("ping should succeed");
        assert!(result);
        mock.shutdown();
    }

    #[tokio::test]
    async fn ping_socket_not_found() {
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("nonexistent.sock");
        let client = ChClient::new(sock);
        let err = client.ping().await.unwrap_err();
        assert!(matches!(err, ClientError::SocketNotFound { .. }));
    }

    #[tokio::test]
    async fn ping_timeout() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "GET",
            "/vmm.ping",
            MockResponse::delayed(Duration::from_secs(5)),
        );
        mock.start().await;

        let client = ChClient::with_timeout(sock, Duration::from_secs(1));
        let err = client.ping().await.unwrap_err();
        assert!(matches!(err, ClientError::Timeout { .. }));
        mock.shutdown();
    }

    // ── info tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn info_returns_parsed_json() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        let vm_json = r#"{"state":"Running","vcpus":4,"memory":1073741824}"#;
        mock.route("GET", "/vm.info", MockResponse::ok_json(vm_json));
        mock.start().await;

        let client = ChClient::new(sock);
        let info = client.info().await.expect("info should succeed");
        assert_eq!(info["state"], "Running");
        assert_eq!(info["vcpus"], 4);
        mock.shutdown();
    }

    #[tokio::test]
    async fn info_garbage_body_returns_invalid_response() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "GET",
            "/vm.info",
            MockResponse::ok_json("this is not json {{{{"),
        );
        mock.start().await;

        let client = ChClient::new(sock);
        let err = client.info().await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidResponse { .. }));
        mock.shutdown();
    }

    // ── create tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn create_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.create", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        let config = serde_json::json!({"kernel": "/vmlinux"});
        client.create(config).await.expect("create should succeed");
        mock.shutdown();
    }

    #[tokio::test]
    async fn create_409_is_error_not_idempotent() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.create", MockResponse::error(409));
        mock.start().await;

        let client = ChClient::new(sock);
        let config = serde_json::json!({"kernel": "/vmlinux"});
        let err = client.create(config).await.unwrap_err();
        assert!(matches!(
            err,
            ClientError::UnexpectedStatus { status: 409, .. }
        ));
        mock.shutdown();
    }

    // ── boot tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn boot_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.boot", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        client.boot().await.expect("boot should succeed");
        mock.shutdown();
    }

    #[tokio::test]
    async fn boot_404_vm_not_created_is_error() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.boot", MockResponse::error(404));
        mock.start().await;

        let client = ChClient::new(sock);
        let err = client.boot().await.unwrap_err();
        assert!(matches!(
            err,
            ClientError::UnexpectedStatus { status: 404, .. }
        ));
        mock.shutdown();
    }

    #[tokio::test]
    async fn boot_409_already_booted_is_idempotent_ok() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.boot", MockResponse::error(409));
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .boot()
            .await
            .expect("boot 409 should be treated as success");
        mock.shutdown();
    }

    // ── shutdown tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn shutdown_graceful_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.shutdown", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .shutdown_graceful()
            .await
            .expect("shutdown should succeed");
        mock.shutdown();
    }

    #[tokio::test]
    async fn shutdown_graceful_404_already_stopped_is_idempotent_ok() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.shutdown", MockResponse::error(404));
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .shutdown_graceful()
            .await
            .expect("shutdown 404 should be treated as success");
        mock.shutdown();
    }

    // ── delete tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.delete", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        client.delete().await.expect("delete should succeed");
        mock.shutdown();
    }

    #[tokio::test]
    async fn delete_404_already_deleted_is_idempotent_ok() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.delete", MockResponse::error(404));
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .delete()
            .await
            .expect("delete 404 should be treated as success");
        mock.shutdown();
    }

    // ── resize tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn resize_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.resize", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .resize(Some(4), Some(1_073_741_824))
            .await
            .expect("resize should succeed");
        mock.shutdown();
    }

    #[tokio::test]
    async fn resize_sends_correct_json_body() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.resize", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .resize(Some(8), Some(2_147_483_648))
            .await
            .expect("resize should succeed");

        let captured = mock
            .captured_body("PUT", "/vm.resize")
            .await
            .expect("request body should be captured");
        let json: serde_json::Value =
            serde_json::from_str(&captured).expect("captured body should be valid JSON");
        assert_eq!(json["desired_vcpus"], 8);
        assert_eq!(json["desired_ram"], 2_147_483_648_u64);
        mock.shutdown();
    }

    // ── counters tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn counters_returns_parsed_json() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        let counters_json = r#"{"vcpu":{"cycles":1000}}"#;
        mock.route("GET", "/vm.counters", MockResponse::ok_json(counters_json));
        mock.start().await;

        let client = ChClient::new(sock);
        let counters = client.counters().await.expect("counters should succeed");
        assert_eq!(counters["vcpu"]["cycles"], 1000);
        mock.shutdown();
    }

    // ── add_disk tests (#1199) ────────────────────────────────────

    #[tokio::test]
    async fn add_disk_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.add-disk", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        let config =
            serde_json::json!({"path": "/dev/nbd0", "readonly": false, "id": "_disk_vol-01"});
        let result = client
            .add_disk(config)
            .await
            .expect("add_disk should succeed");
        assert!(result.is_none()); // 204 = no body
        mock.shutdown();
    }

    #[tokio::test]
    async fn add_disk_returns_body_on_200() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "PUT",
            "/vm.add-disk",
            MockResponse::ok_json(r#"{"id":"_disk_vol-01"}"#),
        );
        mock.start().await;

        let client = ChClient::new(sock);
        let config =
            serde_json::json!({"path": "/dev/nbd0", "readonly": false, "id": "_disk_vol-01"});
        let result = client
            .add_disk(config)
            .await
            .expect("add_disk should succeed");
        assert!(result.is_some());
        assert_eq!(result.unwrap()["id"], "_disk_vol-01");
        mock.shutdown();
    }

    #[tokio::test]
    async fn add_disk_409_is_error() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.add-disk", MockResponse::error(409));
        mock.start().await;

        let client = ChClient::new(sock);
        let config = serde_json::json!({"path": "/dev/nbd0"});
        let err = client.add_disk(config).await.unwrap_err();
        assert!(matches!(
            err,
            ClientError::UnexpectedStatus { status: 409, .. }
        ));
        mock.shutdown();
    }

    // ── remove_device tests (#1199) ─────────────────────────────────

    #[tokio::test]
    async fn remove_device_returns_ok_on_204() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.remove-device", MockResponse::no_content());
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .remove_device("_disk_vol-01")
            .await
            .expect("remove_device should succeed");
        mock.shutdown();
    }

    #[tokio::test]
    async fn remove_device_404_is_idempotent() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.remove-device", MockResponse::error(404));
        mock.start().await;

        let client = ChClient::new(sock);
        client
            .remove_device("_disk_vol-gone")
            .await
            .expect("remove_device 404 should be treated as success");
        mock.shutdown();
    }

    #[tokio::test]
    async fn remove_device_500_is_error() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route("PUT", "/vm.remove-device", MockResponse::error(500));
        mock.start().await;

        let client = ChClient::new(sock);
        let err = client.remove_device("_disk_vol-01").await.unwrap_err();
        assert!(matches!(
            err,
            ClientError::UnexpectedStatus { status: 500, .. }
        ));
        mock.shutdown();
    }

    // ── error handling tests ────────────────────────────────────────

    #[tokio::test]
    async fn connection_refused_no_server_listening() {
        let (_dir, sock) = temp_socket();
        // Create the socket file so SocketNotFound is not triggered, but
        // nobody is listening. We bind+close to leave a stale socket.
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        drop(listener);
        // The socket file still exists but nobody is listening.
        let client = ChClient::new(sock);
        let err = client.ping().await.unwrap_err();
        assert!(
            matches!(err, ClientError::ConnectionRefused)
                || matches!(err, ClientError::InvalidResponse { .. })
        );
    }

    #[tokio::test]
    async fn server_returns_500_unexpected_status() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "GET",
            "/vm.info",
            MockResponse::error_with_body(500, r#"{"error":"internal"}"#),
        );
        mock.start().await;

        let client = ChClient::new(sock);
        let err = client.info().await.unwrap_err();
        assert!(matches!(
            err,
            ClientError::UnexpectedStatus { status: 500, .. }
        ));
        mock.shutdown();
    }

    #[tokio::test]
    async fn timeout_on_slow_server() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "GET",
            "/vm.info",
            MockResponse::delayed(Duration::from_secs(5)),
        );
        mock.start().await;

        let client = ChClient::with_timeout(sock, Duration::from_secs(1));
        let err = client.info().await.unwrap_err();
        assert!(matches!(err, ClientError::Timeout { .. }));
        mock.shutdown();
    }

    // ── concurrent requests ─────────────────────────────────────────

    #[tokio::test]
    async fn concurrent_clients_both_succeed() {
        let (_dir, sock) = temp_socket();
        let mut mock = MockChServer::new(&sock);
        mock.route(
            "GET",
            "/vmm.ping",
            MockResponse::ok_json(r#"{"pong":true}"#),
        );
        mock.start().await;

        let sock_path = sock.clone();
        let (r1, r2) = tokio::join!(
            async {
                let c = ChClient::new(sock_path.clone());
                c.ping().await
            },
            async {
                let c = ChClient::new(sock.clone());
                c.ping().await
            },
        );
        assert!(r1.unwrap());
        assert!(r2.unwrap());
        mock.shutdown();
    }
}
