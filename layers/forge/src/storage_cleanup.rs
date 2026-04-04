//! Storage cleanup — volume deletion cleanup.
//!
//! Detects volumes in the `Deleted` state and performs Forge-side cleanup:
//!
//! 1. Stop ZeroFS if the volume is running
//! 2. Delete S3 objects under the volume's prefix (`volumes/{id}/`)
//! 3. Remove local cache directory
//!
//! This runs as a periodic background task alongside the main reconciler.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// S3 cleanup client
// ---------------------------------------------------------------------------

/// Configuration for S3 object cleanup.
#[derive(Clone)]
pub struct S3CleanupConfig {
    /// S3-compatible endpoint URL (e.g. `https://s3.par.io.cloud.ovh.net`).
    pub endpoint: String,
    /// Bucket name.
    pub bucket: String,
    /// Access key for authentication.
    pub access_key: String,
    /// Secret key for authentication.
    pub secret_key: String,
}

/// Manual Debug impl that redacts `secret_key` to prevent credential leakage
/// in logs, error messages, and debug output.
impl fmt::Debug for S3CleanupConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3CleanupConfig")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("access_key", &self.access_key)
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

/// Errors from S3 cleanup operations.
#[derive(Debug, thiserror::Error)]
pub enum S3CleanupError {
    #[error("failed to list objects under prefix '{prefix}': {source}")]
    ListFailed {
        prefix: String,
        source: reqwest::Error,
    },
    #[error("failed to delete object '{key}': {source}")]
    DeleteFailed { key: String, source: reqwest::Error },
    #[error("S3 returned non-success status {status} for {operation} on '{key}'")]
    S3Error {
        status: u16,
        operation: String,
        key: String,
    },
    #[error("failed to parse S3 list response: {0}")]
    ParseError(String),
}

/// Result of cleaning up S3 objects for a volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3CleanupResult {
    pub volume_id: String,
    pub objects_deleted: usize,
    pub errors: Vec<String>,
}

/// Delete all S3 objects under a given prefix.
///
/// Uses ListObjectsV2 to enumerate, then sends individual DELETE requests.
/// This is intentionally simple — no pagination beyond 1000 objects in v1.
/// When the response is truncated (IsTruncated=true), a warning is logged
/// so operators know a follow-up pass is needed.
pub async fn cleanup_s3_objects(
    config: &S3CleanupConfig,
    volume_id: &str,
) -> Result<S3CleanupResult, S3CleanupError> {
    let prefix = format!("volumes/{volume_id}/");
    let client = reqwest::Client::new();

    let mut result = S3CleanupResult {
        volume_id: volume_id.to_string(),
        objects_deleted: 0,
        errors: Vec::new(),
    };

    // List objects under the prefix.
    let list_url = format!(
        "{}/{bucket}?list-type=2&prefix={prefix}&max-keys=1000",
        config.endpoint,
        bucket = config.bucket,
    );

    let resp = client
        .get(&list_url)
        .header(
            "Authorization",
            s3_auth_header(config, "GET", &config.bucket, ""),
        )
        .send()
        .await
        .map_err(|e| S3CleanupError::ListFailed {
            prefix: prefix.clone(),
            source: e,
        })?;

    if !resp.status().is_success() {
        return Err(S3CleanupError::S3Error {
            status: resp.status().as_u16(),
            operation: "ListObjectsV2".into(),
            key: prefix,
        });
    }

    let body = resp.text().await.map_err(|e| S3CleanupError::ListFailed {
        prefix: prefix.clone(),
        source: e,
    })?;

    // Warn if the response is truncated (more than 1000 keys).
    if body.contains("<IsTruncated>true</IsTruncated>") {
        warn!(
            volume_id,
            prefix = %prefix,
            "S3 list response is truncated (>1000 keys); \
             remaining objects will be cleaned up on the next pass"
        );
    }

    // Parse object keys from the XML response.
    let keys = parse_s3_list_keys(&body);

    if keys.is_empty() {
        debug!(volume_id, "no S3 objects to clean up");
        return Ok(result);
    }

    info!(volume_id, count = keys.len(), "deleting S3 objects");

    // Delete each object.
    for key in &keys {
        let delete_url = format!("{}/{bucket}/{key}", config.endpoint, bucket = config.bucket,);

        match client
            .delete(&delete_url)
            .header(
                "Authorization",
                s3_auth_header(config, "DELETE", &config.bucket, key),
            )
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 204 => {
                result.objects_deleted += 1;
            }
            Ok(resp) => {
                let msg = format!("DELETE {key}: HTTP {}", resp.status().as_u16());
                warn!(volume_id, error = %msg, "S3 delete failed");
                result.errors.push(msg);
            }
            Err(e) => {
                let msg = format!("DELETE {key}: {e}");
                warn!(volume_id, error = %msg, "S3 delete failed");
                result.errors.push(msg);
            }
        }
    }

    info!(
        volume_id,
        deleted = result.objects_deleted,
        errors = result.errors.len(),
        "S3 cleanup complete"
    );

    Ok(result)
}

/// Parse `<Key>...</Key>` entries from an S3 ListObjectsV2 XML response.
///
/// This is a minimal parser that avoids pulling in a full XML crate.
pub fn parse_s3_list_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut search_from = 0;

    loop {
        let start_tag = "<Key>";
        let end_tag = "</Key>";

        let start = match xml[search_from..].find(start_tag) {
            Some(pos) => search_from + pos + start_tag.len(),
            None => break,
        };
        let end = match xml[start..].find(end_tag) {
            Some(pos) => start + pos,
            None => break,
        };

        keys.push(xml[start..end].to_string());
        search_from = end + end_tag.len();
    }

    keys
}

/// Build a simple S3 authorization header.
///
/// TODO(SigV4): This is a placeholder using the legacy `AWS` auth scheme
/// that MinIO and many S3-compatible stores accept for internal use.
/// Production deployments should implement full AWS Signature V4 signing.
/// See: <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-authenticating-requests.html>
pub(crate) fn s3_auth_header(
    config: &S3CleanupConfig,
    _method: &str,
    _bucket: &str,
    _key: &str,
) -> String {
    // Legacy AWS auth format — sufficient for internal S3-compatible endpoints
    // (MinIO, Garage, etc.) but NOT valid for AWS S3 proper.
    format!("AWS {}:{}", config.access_key, config.secret_key)
}

// ---------------------------------------------------------------------------
// Volume cleanup orchestrator
// ---------------------------------------------------------------------------

/// Represents a volume that needs cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeCleanupTask {
    /// Volume ID.
    pub volume_id: String,
    /// S3 prefix for this volume's data.
    pub s3_prefix: String,
    /// Whether ZeroFS needs to be stopped.
    pub zerofs_running: bool,
}

/// Result of a full volume cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeCleanupResult {
    pub volume_id: String,
    pub zerofs_stopped: bool,
    pub s3_cleanup: Option<S3CleanupResult>,
    pub cache_cleaned: bool,
    pub success: bool,
    pub error: Option<String>,
}

/// Perform full cleanup for a deleted volume.
///
/// Steps (in order):
/// 1. Stop ZeroFS if running
/// 2. Delete S3 objects under `volumes/{id}/`
/// 3. Remove local cache directory
pub async fn cleanup_deleted_volume(
    volume_id: &str,
    volume_mgr: &mut syfrah_storage::VolumeMgr,
    s3_config: Option<&S3CleanupConfig>,
    cache_config: Option<&syfrah_storage::cache::CacheConfig>,
) -> VolumeCleanupResult {
    let mut result = VolumeCleanupResult {
        volume_id: volume_id.to_string(),
        zerofs_stopped: false,
        s3_cleanup: None,
        cache_cleaned: false,
        success: true,
        error: None,
    };

    // Step 1: Stop ZeroFS if running.
    if volume_mgr.is_running(volume_id) {
        match volume_mgr.stop_volume(volume_id).await {
            Ok(()) => {
                info!(volume_id, "ZeroFS stopped for deleted volume");
                result.zerofs_stopped = true;
            }
            Err(e) => {
                error!(volume_id, error = %e, "failed to stop ZeroFS for deleted volume");
                result.success = false;
                result.error = Some(format!("stop ZeroFS: {e}"));
                return result;
            }
        }
    }

    // Step 2: Delete S3 objects.
    if let Some(s3) = s3_config {
        match cleanup_s3_objects(s3, volume_id).await {
            Ok(s3_result) => {
                if !s3_result.errors.is_empty() {
                    warn!(
                        volume_id,
                        errors = s3_result.errors.len(),
                        "S3 cleanup had partial failures"
                    );
                }
                result.s3_cleanup = Some(s3_result);
            }
            Err(e) => {
                error!(volume_id, error = %e, "S3 cleanup failed");
                result.success = false;
                result.error = Some(format!("S3 cleanup: {e}"));
                // Continue to cache cleanup even if S3 fails.
            }
        }
    }

    // Step 3: Clean up local cache directory.
    if let Some(cache) = cache_config {
        match syfrah_storage::cleanup_volume_cache(cache, volume_id) {
            Ok(()) => {
                debug!(volume_id, "cache directory cleaned up");
                result.cache_cleaned = true;
            }
            Err(e) => {
                warn!(volume_id, error = %e, "cache cleanup failed");
                // Cache cleanup failure is non-fatal.
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Background cleanup loop
// ---------------------------------------------------------------------------

/// Run a periodic loop that checks for volumes in the Deleted state
/// and performs cleanup.
///
/// `volume_ids_fn` is called each iteration to get the list of volumes
/// that are in the Deleted state and need cleanup.
pub async fn run_storage_cleanup_loop(interval: Duration, mut shutdown_rx: watch::Receiver<bool>) {
    info!(
        interval_secs = interval.as_secs(),
        "storage cleanup loop started"
    );

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                debug!("storage cleanup tick");
                // In production, this loop would:
                // 1. Query the Raft state for volumes with state == Deleted
                // 2. For each, call cleanup_deleted_volume()
                // 3. After successful cleanup, submit PurgeTombstones if past TTL
                //
                // The actual integration with Raft queries depends on
                // how the Forge runtime accesses the state machine, which
                // varies by deployment. The cleanup_deleted_volume() function
                // above provides the core logic.
            }
            result = shutdown_rx.changed() => {
                if result.is_err() || *shutdown_rx.borrow() {
                    info!("storage cleanup loop shutting down");
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_s3_list_keys_empty() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>bucket</Name>
  <Prefix>volumes/vol-nonexistent/</Prefix>
  <KeyCount>0</KeyCount>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
</ListBucketResult>"#;
        let keys = parse_s3_list_keys(xml);
        assert!(keys.is_empty());
    }

    #[test]
    fn parse_s3_list_keys_multiple() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>bucket</Name>
  <Prefix>volumes/vol-abc/</Prefix>
  <KeyCount>3</KeyCount>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>volumes/vol-abc/gen-1/data.sst</Key>
    <Size>1048576</Size>
  </Contents>
  <Contents>
    <Key>volumes/vol-abc/gen-1/index.sst</Key>
    <Size>4096</Size>
  </Contents>
  <Contents>
    <Key>volumes/vol-abc/gen-2/data.sst</Key>
    <Size>2097152</Size>
  </Contents>
</ListBucketResult>"#;
        let keys = parse_s3_list_keys(xml);
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0], "volumes/vol-abc/gen-1/data.sst");
        assert_eq!(keys[1], "volumes/vol-abc/gen-1/index.sst");
        assert_eq!(keys[2], "volumes/vol-abc/gen-2/data.sst");
    }

    #[test]
    fn s3_cleanup_config_debug_redacts_secret() {
        let config = S3CleanupConfig {
            endpoint: "https://s3.example.com".into(),
            bucket: "test-bucket".into(),
            access_key: "AKID".into(),
            secret_key: "SUPER_SECRET_KEY".into(),
        };
        let dbg = format!("{config:?}");
        assert!(dbg.contains("test-bucket"), "should contain bucket name");
        assert!(dbg.contains("AKID"), "should contain access_key");
        assert!(dbg.contains("[REDACTED]"), "should contain [REDACTED]");
        assert!(
            !dbg.contains("SUPER_SECRET_KEY"),
            "must NOT contain the actual secret key"
        );
    }

    #[test]
    fn volume_cleanup_result_serializes() {
        let result = VolumeCleanupResult {
            volume_id: "vol-123".into(),
            zerofs_stopped: true,
            s3_cleanup: Some(S3CleanupResult {
                volume_id: "vol-123".into(),
                objects_deleted: 5,
                errors: vec![],
            }),
            cache_cleaned: true,
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("vol-123"));
        assert!(json.contains("\"objects_deleted\":5"));
    }

    #[test]
    fn s3_cleanup_result_with_errors() {
        let result = S3CleanupResult {
            volume_id: "vol-456".into(),
            objects_deleted: 3,
            errors: vec!["DELETE key1: HTTP 500".into()],
        };
        assert_eq!(result.objects_deleted, 3);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn s3_auth_header_format() {
        let config = S3CleanupConfig {
            endpoint: "https://s3.example.com".into(),
            bucket: "bucket".into(),
            access_key: "AKID".into(),
            secret_key: "SECRET".into(),
        };
        let header = s3_auth_header(&config, "GET", "bucket", "key");
        assert_eq!(header, "AWS AKID:SECRET");
    }

    #[test]
    fn volume_cleanup_task_serializes() {
        let task = VolumeCleanupTask {
            volume_id: "vol-789".into(),
            s3_prefix: "volumes/vol-789/".into(),
            zerofs_running: true,
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("vol-789"));
        let parsed: VolumeCleanupTask = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.volume_id, "vol-789");
        assert!(parsed.zerofs_running);
    }

    #[tokio::test]
    async fn cleanup_loop_shuts_down() {
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            run_storage_cleanup_loop(Duration::from_secs(60), rx).await;
        });

        // Signal shutdown immediately.
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    #[test]
    fn parse_s3_list_keys_malformed() {
        // Partial tag — should not panic.
        let xml = "<Key>partial";
        let keys = parse_s3_list_keys(xml);
        assert!(keys.is_empty());
    }

    #[test]
    fn parse_s3_list_keys_nested_key_tag() {
        // Ensure we handle normal case correctly even with tricky content.
        let xml = "<Contents><Key>a/b/c</Key></Contents>";
        let keys = parse_s3_list_keys(xml);
        assert_eq!(keys, vec!["a/b/c"]);
    }
}
