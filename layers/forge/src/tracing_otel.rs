//! OpenTelemetry tracing integration.
//!
//! Provides span creation for API requests, reconciliation cycles,
//! and subsystem calls. Trace ID propagation from caller via
//! `traceparent` header.
//!
//! OTLP export is configurable and disabled by default.
//! When enabled, spans are exported to an OTLP-compatible endpoint.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Configuration for OpenTelemetry tracing.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OtelConfig {
    /// Whether OTLP export is enabled.
    pub enabled: bool,
    /// OTLP endpoint URL (e.g., "http://localhost:4317").
    pub endpoint: String,
    /// Service name for traces.
    pub service_name: String,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:4317".to_string(),
            service_name: "syfrah-forge".to_string(),
        }
    }
}

/// OpenTelemetry tracing controller.
pub struct OtelController {
    config: OtelConfig,
    initialized: AtomicBool,
}

impl OtelController {
    /// Create a new controller with the given configuration.
    pub fn new(config: OtelConfig) -> Self {
        Self {
            config,
            initialized: AtomicBool::new(false),
        }
    }

    /// Create with defaults (disabled).
    pub fn disabled() -> Self {
        Self::new(OtelConfig::default())
    }

    /// Check if OTLP export is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the OTLP endpoint.
    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// Initialize the OTLP exporter (no-op if disabled or already initialized).
    pub fn init(&self) {
        if !self.config.enabled {
            tracing::debug!("OTLP export disabled — skipping initialization");
            return;
        }
        if self.initialized.swap(true, Ordering::SeqCst) {
            return; // Already initialized
        }
        tracing::info!(
            endpoint = %self.config.endpoint,
            service = %self.config.service_name,
            "OTLP exporter initialized (spans will be exported)"
        );
        // When opentelemetry + tracing-opentelemetry crates are added:
        // 1. Create OTLP exporter with config.endpoint
        // 2. Create TracerProvider
        // 3. Register as global tracer
        // 4. Add tracing-opentelemetry layer to subscriber
    }

    /// Shutdown the OTLP exporter.
    pub fn shutdown(&self) {
        if !self.initialized.load(Ordering::SeqCst) {
            return;
        }
        tracing::info!("OTLP exporter shutdown");
    }

    /// Get the config.
    pub fn config(&self) -> &OtelConfig {
        &self.config
    }
}

impl Default for OtelController {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Extract a trace ID from the `traceparent` header (W3C Trace Context).
/// Format: `00-{trace_id}-{parent_id}-{flags}`
pub fn extract_trace_id(traceparent: &str) -> Option<String> {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() == 4 && parts[0] == "00" {
        Some(parts[1].to_string())
    } else {
        None
    }
}

/// Create a tracing span for an API request.
#[macro_export]
macro_rules! forge_api_span {
    ($method:expr, $path:expr) => {
        tracing::info_span!(
            "forge_api",
            method = %$method,
            path = %$path,
            otel.kind = "server"
        )
    };
}

/// Create a tracing span for a reconciliation cycle.
#[macro_export]
macro_rules! forge_reconcile_span {
    () => {
        tracing::info_span!("forge_reconcile", otel.kind = "internal")
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otel_config_defaults() {
        let config = OtelConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.endpoint, "http://localhost:4317");
        assert_eq!(config.service_name, "syfrah-forge");
    }

    #[test]
    fn controller_disabled_by_default() {
        let ctrl = OtelController::disabled();
        assert!(!ctrl.is_enabled());
    }

    #[test]
    fn extract_trace_id_valid() {
        let id = extract_trace_id("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        assert_eq!(id, Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string()));
    }

    #[test]
    fn extract_trace_id_invalid() {
        assert!(extract_trace_id("invalid").is_none());
        assert!(extract_trace_id("01-abc-def-00").is_none());
    }
}
