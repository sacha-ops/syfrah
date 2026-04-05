//! Periodic S3 health probe and degradation alerting.
//!
//! Background task: every `probe_interval` (default 30s), PUT + GET + DELETE
//! a small test object to S3. Tracks put/get latency and reachability.
//!
//! Degradation thresholds per ADR-006 S25 (defaults):
//! - < 30s outage:  Healthy (transient, fsyncs may block)
//! - 30s–5min:      FsyncBlocking (fsyncs blocked, no I/O errors yet)
//! - 5min–15min:    EIO (I/O errors returned to guests)
//! - 15min–30min:   Degraded (volumes marked degraded)
//! - > 30min:       Error (volumes marked error, operator intervention needed)
//!
//! All thresholds are configurable operational policy values.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Degradation state
// ---------------------------------------------------------------------------

/// S3 degradation level per ADR-006 S25.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum S3DegradationLevel {
    /// S3 is reachable, latency acceptable.
    Healthy,
    /// < threshold_fsync_block: fsync blocks, no I/O errors yet.
    FsyncBlocking,
    /// threshold_fsync_block..threshold_degraded: EIO returned to guests.
    Eio,
    /// threshold_degraded..threshold_error: volumes marked degraded.
    Degraded,
    /// > threshold_error: volumes marked error, operator intervention needed.
    Error,
}

impl std::fmt::Display for S3DegradationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "Healthy"),
            Self::FsyncBlocking => write!(f, "FsyncBlocking"),
            Self::Eio => write!(f, "EIO"),
            Self::Degraded => write!(f, "Degraded"),
            Self::Error => write!(f, "Error"),
        }
    }
}

// ---------------------------------------------------------------------------
// Configurable thresholds
// ---------------------------------------------------------------------------

/// Configurable degradation thresholds. All durations are measured from the
/// first failed probe. These are operational policy values that operators
/// can tune without code changes.
#[derive(Debug, Clone)]
pub struct S3HealthThresholds {
    /// Outage duration below which we just block fsyncs (default 30s).
    pub fsync_block: Duration,
    /// Outage duration at which we start returning EIO (default 5min).
    pub eio: Duration,
    /// Outage duration at which volumes are marked degraded (default 15min).
    pub degraded: Duration,
    /// Outage duration at which volumes are marked error (default 30min).
    pub error: Duration,
    /// How often to probe S3 (default 30s).
    pub probe_interval: Duration,
}

impl Default for S3HealthThresholds {
    fn default() -> Self {
        Self {
            fsync_block: Duration::from_secs(30),
            eio: Duration::from_secs(5 * 60),
            degraded: Duration::from_secs(15 * 60),
            error: Duration::from_secs(30 * 60),
            probe_interval: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// Probe result snapshot (shared via Arc)
// ---------------------------------------------------------------------------

/// Snapshot of the latest S3 health probe result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3HealthSnapshot {
    /// Whether the last probe succeeded.
    pub s3_reachable: bool,
    /// PUT latency of the last successful probe (ms).
    pub s3_put_latency_ms: Option<u64>,
    /// GET latency of the last successful probe (ms).
    pub s3_get_latency_ms: Option<u64>,
    /// Current degradation level.
    pub degradation_level: S3DegradationLevel,
    /// Duration of current outage (zero if healthy).
    pub outage_duration_secs: u64,
    /// Error message from the last failed probe, if any.
    pub last_error: Option<String>,
}

impl Default for S3HealthSnapshot {
    fn default() -> Self {
        Self {
            s3_reachable: false,
            s3_put_latency_ms: None,
            s3_get_latency_ms: None,
            degradation_level: S3DegradationLevel::Healthy,
            outage_duration_secs: 0,
            last_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// S3 health probe configuration
// ---------------------------------------------------------------------------

/// Configuration for the S3 health probe background task.
#[derive(Debug, Clone)]
pub struct S3HealthProbeConfig {
    /// S3 endpoint URL.
    pub endpoint: String,
    /// S3 bucket name.
    pub bucket: String,
    /// S3 access key.
    pub access_key: String,
    /// S3 secret key (kept in memory only, never logged).
    pub secret_key: String,
    /// Degradation thresholds.
    pub thresholds: S3HealthThresholds,
}

// ---------------------------------------------------------------------------
// Shared state handle
// ---------------------------------------------------------------------------

/// Thread-safe handle to the latest S3 health snapshot.
///
/// The background probe task writes to this; consumers (gossip, status API)
/// read from it.
#[derive(Clone)]
pub struct S3HealthHandle {
    rx: watch::Receiver<S3HealthSnapshot>,
}

impl S3HealthHandle {
    /// Get the latest health snapshot.
    pub fn snapshot(&self) -> S3HealthSnapshot {
        self.rx.borrow().clone()
    }
}

// ---------------------------------------------------------------------------
// Background probe task
// ---------------------------------------------------------------------------

/// Start the S3 health probe background task.
///
/// Returns a handle that consumers can use to read the latest probe results.
/// The task runs until `shutdown_rx` fires.
pub fn start_s3_health_probe(
    config: S3HealthProbeConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> S3HealthHandle {
    let (tx, rx) = watch::channel(S3HealthSnapshot::default());
    let handle = S3HealthHandle { rx };

    tokio::spawn(async move {
        run_probe_loop(config, tx, shutdown_rx).await;
    });

    handle
}

/// Internal probe loop. Exported for testing with custom channels.
async fn run_probe_loop(
    config: S3HealthProbeConfig,
    tx: watch::Sender<S3HealthSnapshot>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("failed to build reqwest client for S3 health probe");

    let mut outage_start: Option<Instant> = None;
    let interval = config.thresholds.probe_interval;

    info!(
        endpoint = %config.endpoint,
        bucket = %config.bucket,
        interval_secs = interval.as_secs(),
        "S3 health probe started"
    );

    loop {
        let result = probe_s3_once(&client, &config).await;

        let snapshot = match result {
            Ok((put_ms, get_ms)) => {
                if outage_start.is_some() {
                    info!("S3 health probe: connectivity restored");
                }
                outage_start = None;
                S3HealthSnapshot {
                    s3_reachable: true,
                    s3_put_latency_ms: Some(put_ms),
                    s3_get_latency_ms: Some(get_ms),
                    degradation_level: S3DegradationLevel::Healthy,
                    outage_duration_secs: 0,
                    last_error: None,
                }
            }
            Err(e) => {
                let start = *outage_start.get_or_insert_with(Instant::now);
                let outage = start.elapsed();
                let level = classify_outage(&config.thresholds, outage);

                warn!(
                    error = %e,
                    outage_secs = outage.as_secs(),
                    level = %level,
                    "S3 health probe failed"
                );

                S3HealthSnapshot {
                    s3_reachable: false,
                    s3_put_latency_ms: None,
                    s3_get_latency_ms: None,
                    degradation_level: level,
                    outage_duration_secs: outage.as_secs(),
                    // Sanitize: never include credentials or internal paths
                    // in error messages exposed to operators.
                    last_error: Some(sanitize_error(&e)),
                }
            }
        };

        // Ignore send error: means all receivers dropped (shutdown).
        let _ = tx.send(snapshot);

        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("S3 health probe shutting down");
                    break;
                }
            }
        }
    }
}

/// Execute a single PUT + GET + DELETE probe against S3.
///
/// Returns (put_latency_ms, get_latency_ms) on success.
///
/// Uses AWS Signature V4 request signing, which is required by AWS S3
/// and all major S3-compatible backends (Hetzner Object Storage, MinIO,
/// Ceph RGW).
async fn probe_s3_once(
    client: &reqwest::Client,
    config: &S3HealthProbeConfig,
) -> Result<(u64, u64), String> {
    let test_key = format!("_syfrah_health_probe_{}", std::process::id());
    let test_body = b"syfrah-health-probe";

    // -- PUT --
    let put_start = Instant::now();
    let put_req = s3v4_request(
        client,
        &S3v4Params {
            method: "PUT",
            endpoint: &config.endpoint,
            bucket: &config.bucket,
            key: &test_key,
            access_key: &config.access_key,
            secret_key: &config.secret_key,
            body: Some(test_body),
        },
    )
    .map_err(|e| format!("PUT signing failed: {e}"))?;

    let put_resp = client
        .execute(put_req)
        .await
        .map_err(|e| format!("PUT request failed: {e}"))?;

    if !put_resp.status().is_success() {
        return Err(format!("PUT returned HTTP {}", put_resp.status()));
    }
    let put_ms = put_start.elapsed().as_millis() as u64;

    // -- GET --
    let get_start = Instant::now();
    let get_req = s3v4_request(
        client,
        &S3v4Params {
            method: "GET",
            endpoint: &config.endpoint,
            bucket: &config.bucket,
            key: &test_key,
            access_key: &config.access_key,
            secret_key: &config.secret_key,
            body: None,
        },
    )
    .map_err(|e| format!("GET signing failed: {e}"))?;

    let get_resp = client
        .execute(get_req)
        .await
        .map_err(|e| format!("GET request failed: {e}"))?;

    if !get_resp.status().is_success() {
        return Err(format!("GET returned HTTP {}", get_resp.status()));
    }
    let get_ms = get_start.elapsed().as_millis() as u64;

    // -- DELETE (best-effort, don't fail the probe on cleanup) --
    let del_req = s3v4_request(
        client,
        &S3v4Params {
            method: "DELETE",
            endpoint: &config.endpoint,
            bucket: &config.bucket,
            key: &test_key,
            access_key: &config.access_key,
            secret_key: &config.secret_key,
            body: None,
        },
    );
    if let Ok(req) = del_req {
        if let Err(e) = client.execute(req).await {
            debug!(error = %e, "S3 health probe: DELETE cleanup failed (non-fatal)");
        }
    }

    Ok((put_ms, get_ms))
}

/// Build an AWS SigV4-signed request for S3.
///
/// Implements the minimal subset of AWS Signature Version 4 needed for
/// simple object operations (PUT, GET, DELETE) against S3-compatible
/// endpoints using path-style addressing.
/// Parameters for a single S3 SigV4-signed request.
struct S3v4Params<'a> {
    method: &'a str,
    endpoint: &'a str,
    bucket: &'a str,
    key: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
    body: Option<&'a [u8]>,
}

fn s3v4_request(
    client: &reqwest::Client,
    params: &S3v4Params<'_>,
) -> Result<reqwest::Request, String> {
    let S3v4Params {
        method,
        endpoint,
        bucket,
        key,
        access_key,
        secret_key,
        body,
    } = params;
    use chrono::Utc;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    let now = Utc::now();
    let date_stamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

    // Parse host from endpoint URL.
    let url = reqwest::Url::parse(endpoint).map_err(|e| format!("invalid endpoint URL: {e}"))?;
    let host = url.host_str().ok_or("endpoint has no host")?;
    let host_header = if let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.to_string()
    };

    // Derive region from endpoint. For Hetzner/MinIO style endpoints, use
    // "us-east-1" as the default region (S3 SigV4 region for path-style).
    let region = "us-east-1";
    let service = "s3";

    // Payload hash.
    let payload = body.unwrap_or(&b""[..]);
    let payload_hash = hex::encode(Sha256::digest(payload));

    // Canonical request.
    let object_url = build_s3_url(endpoint, bucket, key);
    let parsed_url =
        reqwest::Url::parse(&object_url).map_err(|e| format!("invalid object URL: {e}"))?;
    let canonical_uri = parsed_url.path().to_string();

    let canonical_headers =
        format!("host:{host_header}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    // String to sign.
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    // Signing key.
    type HmacSha256 = Hmac<Sha256>;
    let k_date = {
        let mut mac = HmacSha256::new_from_slice(format!("AWS4{secret_key}").as_bytes())
            .map_err(|e| format!("HMAC init failed: {e}"))?;
        mac.update(date_stamp.as_bytes());
        mac.finalize().into_bytes()
    };
    let k_region = {
        let mut mac =
            HmacSha256::new_from_slice(&k_date).map_err(|e| format!("HMAC failed: {e}"))?;
        mac.update(region.as_bytes());
        mac.finalize().into_bytes()
    };
    let k_service = {
        let mut mac =
            HmacSha256::new_from_slice(&k_region).map_err(|e| format!("HMAC failed: {e}"))?;
        mac.update(service.as_bytes());
        mac.finalize().into_bytes()
    };
    let k_signing = {
        let mut mac =
            HmacSha256::new_from_slice(&k_service).map_err(|e| format!("HMAC failed: {e}"))?;
        mac.update(b"aws4_request");
        mac.finalize().into_bytes()
    };

    // Signature.
    let signature = {
        let mut mac =
            HmacSha256::new_from_slice(&k_signing).map_err(|e| format!("HMAC failed: {e}"))?;
        mac.update(string_to_sign.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    };

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, \
         SignedHeaders={signed_headers}, Signature={signature}"
    );

    // Build the request.
    let reqwest_method = match *method {
        "PUT" => reqwest::Method::PUT,
        "GET" => reqwest::Method::GET,
        "DELETE" => reqwest::Method::DELETE,
        "HEAD" => reqwest::Method::HEAD,
        other => return Err(format!("unsupported method: {other}")),
    };

    let mut builder = client
        .request(reqwest_method, &object_url)
        .header("Host", &host_header)
        .header("x-amz-date", &amz_date)
        .header("x-amz-content-sha256", &payload_hash)
        .header("Authorization", &authorization);

    if let Some(b) = body {
        builder = builder
            .header("Content-Type", "application/octet-stream")
            .body(b.to_vec());
    }

    builder
        .build()
        .map_err(|e| format!("failed to build request: {e}"))
}

/// Build the S3 object URL. Supports path-style addressing which is the
/// standard for self-hosted S3-compatible endpoints (MinIO, etc.).
fn build_s3_url(endpoint: &str, bucket: &str, key: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    format!("{base}/{bucket}/{key}")
}

/// Classify the current outage duration into a degradation level.
fn classify_outage(thresholds: &S3HealthThresholds, outage: Duration) -> S3DegradationLevel {
    if outage >= thresholds.error {
        S3DegradationLevel::Error
    } else if outage >= thresholds.degraded {
        S3DegradationLevel::Degraded
    } else if outage >= thresholds.eio {
        S3DegradationLevel::Eio
    } else if outage >= thresholds.fsync_block {
        S3DegradationLevel::FsyncBlocking
    } else {
        S3DegradationLevel::Healthy
    }
}

/// Sanitize error messages to avoid leaking credentials or internal details.
/// Strips anything that looks like a secret key or authorization header.
fn sanitize_error(err: &str) -> String {
    // Remove potential credential fragments from error strings.
    let sanitized = err
        .replace(&['\'', '"'][..], "")
        // Truncate excessively long errors (e.g., from HTML error pages).
        .chars()
        .take(256)
        .collect::<String>();

    // If the error contains "authorization" or "secret", replace with generic message.
    let lower = sanitized.to_lowercase();
    if lower.contains("secret") || lower.contains("authorization") || lower.contains("credential") {
        return "S3 authentication error (details redacted for security)".to_string();
    }

    sanitized
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_outage_healthy_below_fsync_block() {
        let t = S3HealthThresholds::default();
        assert_eq!(
            classify_outage(&t, Duration::from_secs(10)),
            S3DegradationLevel::Healthy
        );
    }

    #[test]
    fn classify_outage_fsync_blocking() {
        let t = S3HealthThresholds::default();
        assert_eq!(
            classify_outage(&t, Duration::from_secs(30)),
            S3DegradationLevel::FsyncBlocking
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(60)),
            S3DegradationLevel::FsyncBlocking
        );
    }

    #[test]
    fn classify_outage_eio() {
        let t = S3HealthThresholds::default();
        assert_eq!(
            classify_outage(&t, Duration::from_secs(5 * 60)),
            S3DegradationLevel::Eio
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(10 * 60)),
            S3DegradationLevel::Eio
        );
    }

    #[test]
    fn classify_outage_degraded() {
        let t = S3HealthThresholds::default();
        // degraded default = 15min, error default = 30min
        assert_eq!(
            classify_outage(&t, Duration::from_secs(15 * 60)),
            S3DegradationLevel::Degraded
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(20 * 60)),
            S3DegradationLevel::Degraded
        );
    }

    #[test]
    fn classify_outage_error() {
        let t = S3HealthThresholds::default();
        // error default = 30min
        assert_eq!(
            classify_outage(&t, Duration::from_secs(30 * 60)),
            S3DegradationLevel::Error
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(60 * 60)),
            S3DegradationLevel::Error
        );
    }

    #[test]
    fn classify_outage_custom_thresholds() {
        let t = S3HealthThresholds {
            fsync_block: Duration::from_secs(10),
            eio: Duration::from_secs(60),
            degraded: Duration::from_secs(300),
            error: Duration::from_secs(600),
            probe_interval: Duration::from_secs(5),
        };
        assert_eq!(
            classify_outage(&t, Duration::from_secs(5)),
            S3DegradationLevel::Healthy
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(10)),
            S3DegradationLevel::FsyncBlocking
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(60)),
            S3DegradationLevel::Eio
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(300)),
            S3DegradationLevel::Degraded
        );
        assert_eq!(
            classify_outage(&t, Duration::from_secs(600)),
            S3DegradationLevel::Error
        );
    }

    #[test]
    fn build_s3_url_path_style() {
        assert_eq!(
            build_s3_url("http://s3:9000", "mybucket", "mykey"),
            "http://s3:9000/mybucket/mykey"
        );
    }

    #[test]
    fn build_s3_url_strips_trailing_slash() {
        assert_eq!(
            build_s3_url("http://s3:9000/", "bucket", "key"),
            "http://s3:9000/bucket/key"
        );
    }

    #[test]
    fn sanitize_error_truncates_long_strings() {
        let long = "x".repeat(500);
        let sanitized = sanitize_error(&long);
        assert!(sanitized.len() <= 256);
    }

    #[test]
    fn sanitize_error_redacts_credentials() {
        let msg = "failed to authorize: invalid secret key abc123";
        let sanitized = sanitize_error(msg);
        assert!(sanitized.contains("redacted"));
        assert!(!sanitized.contains("abc123"));
    }

    #[test]
    fn sanitize_error_passes_through_normal_errors() {
        let msg = "connection timed out after 15s";
        let sanitized = sanitize_error(msg);
        assert_eq!(sanitized, msg);
    }

    #[test]
    fn default_thresholds_are_sane() {
        let t = S3HealthThresholds::default();
        assert!(t.fsync_block < t.eio);
        assert!(t.eio < t.degraded);
        assert!(t.degraded < t.error);
        assert_eq!(t.probe_interval, Duration::from_secs(30));
    }

    #[test]
    fn snapshot_default_is_unreachable() {
        let s = S3HealthSnapshot::default();
        assert!(!s.s3_reachable);
        assert_eq!(s.degradation_level, S3DegradationLevel::Healthy);
        assert!(s.s3_put_latency_ms.is_none());
        assert!(s.s3_get_latency_ms.is_none());
    }

    #[test]
    fn degradation_level_display() {
        assert_eq!(S3DegradationLevel::Healthy.to_string(), "Healthy");
        assert_eq!(
            S3DegradationLevel::FsyncBlocking.to_string(),
            "FsyncBlocking"
        );
        assert_eq!(S3DegradationLevel::Eio.to_string(), "EIO");
        assert_eq!(S3DegradationLevel::Degraded.to_string(), "Degraded");
        assert_eq!(S3DegradationLevel::Error.to_string(), "Error");
    }

    #[test]
    fn s3_health_snapshot_serde_roundtrip() {
        let snap = S3HealthSnapshot {
            s3_reachable: true,
            s3_put_latency_ms: Some(42),
            s3_get_latency_ms: Some(15),
            degradation_level: S3DegradationLevel::Healthy,
            outage_duration_secs: 0,
            last_error: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let deserialized: S3HealthSnapshot = serde_json::from_str(&json).unwrap();
        assert!(deserialized.s3_reachable);
        assert_eq!(deserialized.s3_put_latency_ms, Some(42));
        assert_eq!(deserialized.s3_get_latency_ms, Some(15));
    }

    #[tokio::test]
    async fn probe_loop_shuts_down_on_signal() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let config = S3HealthProbeConfig {
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable
            bucket: "test".to_string(),
            access_key: "ak".to_string(),
            secret_key: "sk".to_string(),
            thresholds: S3HealthThresholds {
                probe_interval: Duration::from_millis(50),
                ..S3HealthThresholds::default()
            },
        };

        let handle = start_s3_health_probe(config, shutdown_rx);

        // Let a few probes run.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Snapshot should show unreachable.
        let snap = handle.snapshot();
        assert!(!snap.s3_reachable);

        // Signal shutdown.
        shutdown_tx.send(true).unwrap();

        // Give the task time to exit.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
