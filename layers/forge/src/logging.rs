//! Structured JSON logging for Forge operations.
//!
//! All Forge operations are logged as structured JSON with fields:
//! - timestamp, level, message, request_id, resource_id, operation, duration_ms, result
//! - Reconciliation cycles logged with changes_applied, drift_detected
//!
//! Uses tracing + tracing-subscriber with JSON formatter.

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Structured log entry for Forge operations.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ForgeLogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes_applied: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_detected: Option<bool>,
}

/// Operation timer for measuring duration.
pub struct OperationTimer {
    start: Instant,
    pub operation: String,
    pub resource_id: Option<String>,
    pub request_id: Option<String>,
}

impl OperationTimer {
    /// Start timing an operation.
    pub fn start(operation: &str) -> Self {
        Self {
            start: Instant::now(),
            operation: operation.to_string(),
            resource_id: None,
            request_id: None,
        }
    }

    /// Set the resource ID.
    pub fn with_resource(mut self, id: &str) -> Self {
        self.resource_id = Some(id.to_string());
        self
    }

    /// Set the request ID.
    pub fn with_request(mut self, id: &str) -> Self {
        self.request_id = Some(id.to_string());
        self
    }

    /// Complete the operation and log it.
    pub fn complete(self, result: &str) {
        let duration_ms = self.start.elapsed().as_millis() as u64;
        tracing::info!(
            operation = %self.operation,
            resource_id = ?self.resource_id,
            request_id = ?self.request_id,
            duration_ms = duration_ms,
            result = %result,
            "operation completed"
        );
    }

    /// Complete with an error.
    pub fn fail(self, error: &str) {
        let duration_ms = self.start.elapsed().as_millis() as u64;
        tracing::error!(
            operation = %self.operation,
            resource_id = ?self.resource_id,
            request_id = ?self.request_id,
            duration_ms = duration_ms,
            result = "error",
            error = %error,
            "operation failed"
        );
    }

    /// Get elapsed milliseconds.
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Log a reconciliation cycle.
pub fn log_reconciliation(duration_ms: u64, changes_applied: u32, drift_detected: bool) {
    tracing::info!(
        operation = "reconcile",
        duration_ms = duration_ms,
        changes_applied = changes_applied,
        drift_detected = drift_detected,
        "reconciliation cycle completed"
    );
}

/// Initialize structured JSON logging for Forge.
///
/// This configures tracing-subscriber to output JSON-formatted logs.
/// Should be called once at startup.
pub fn init_json_logging() {
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,syfrah_forge=debug"));

    let json_layer = fmt::layer()
        .json()
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .try_init()
        .ok(); // Ok to fail if already initialized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_timer_tracks_duration() {
        let timer = OperationTimer::start("test_op")
            .with_resource("vm-1")
            .with_request("req-123");

        assert!(timer.elapsed_ms() < 100);
        assert_eq!(timer.operation, "test_op");
        assert_eq!(timer.resource_id.as_deref(), Some("vm-1"));
        assert_eq!(timer.request_id.as_deref(), Some("req-123"));
    }

    #[test]
    fn log_entry_serializes() {
        let entry = ForgeLogEntry {
            timestamp: "2026-04-01T00:00:00Z".to_string(),
            level: "info".to_string(),
            message: "test".to_string(),
            request_id: Some("req-1".to_string()),
            resource_id: Some("vm-1".to_string()),
            operation: Some("create".to_string()),
            duration_ms: Some(42),
            result: Some("success".to_string()),
            changes_applied: None,
            drift_detected: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"request_id\":\"req-1\""));
        assert!(json.contains("\"duration_ms\":42"));
        // Optional None fields should be omitted
        assert!(!json.contains("changes_applied"));
    }
}
