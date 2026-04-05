//! Unified error types for Syfrah.
//!
//! Every layer uses these error types so that error messages are consistent,
//! actionable, and machine-parseable across the entire CLI and API.
//!
//! # Design
//!
//! - **Code**: machine-readable identifier (e.g., `RESOURCE_NOT_FOUND`)
//! - **Message**: human-readable explanation
//! - **Suggestion**: actionable next step (e.g., "Run: syfrah vpc list")
//! - **Context**: structured metadata for debugging
//!
//! ```
//! use syfrah_core::error::SyfrahError;
//!
//! let err = SyfrahError::not_found("vpc", "my-vpc")
//!     .with_suggestion("List available VPCs with: syfrah vpc list");
//! assert_eq!(err.code, "RESOURCE_NOT_FOUND");
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;

/// The unified error type for all of Syfrah.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyfrahError {
    /// Machine-readable error code (e.g., `RESOURCE_NOT_FOUND`)
    pub code: String,
    /// Human-readable error message
    pub message: String,
    /// Actionable suggestion for the operator (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Structured context for debugging (optional)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub context: Vec<(String, String)>,
}

impl SyfrahError {
    /// Create a new error with a code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            suggestion: None,
            context: Vec::new(),
        }
    }

    /// Add a suggestion ("Run: syfrah ...").
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Add a context key-value pair.
    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.push((key.into(), value.into()));
        self
    }

    // ── Common error constructors ──────────────────────────────

    /// Resource not found.
    pub fn not_found(kind: &str, name: &str) -> Self {
        Self::new("RESOURCE_NOT_FOUND", format!("{kind} '{name}' not found"))
            .with_context("resource_kind", kind)
            .with_context("resource_name", name)
    }

    /// Resource already exists.
    pub fn already_exists(kind: &str, name: &str) -> Self {
        Self::new(
            "RESOURCE_ALREADY_EXISTS",
            format!("{kind} '{name}' already exists"),
        )
        .with_context("resource_kind", kind)
        .with_context("resource_name", name)
    }

    /// Validation failed.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new("VALIDATION_ERROR", message)
    }

    /// Invalid name format.
    pub fn invalid_name(name: &str, reason: &str) -> Self {
        Self::new("INVALID_NAME", format!("invalid name '{name}': {reason}"))
            .with_context("name", name)
    }

    /// Permission denied.
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new("PERMISSION_DENIED", message)
    }

    /// Operation not supported or not yet implemented.
    pub fn not_implemented(operation: &str) -> Self {
        Self::new(
            "NOT_IMPLEMENTED",
            format!("'{operation}' is not yet implemented"),
        )
    }

    /// Conflict — resource state prevents the operation.
    pub fn conflict(kind: &str, name: &str, reason: impl Into<String>) -> Self {
        Self::new("CONFLICT", format!("{kind} '{name}': {}", reason.into()))
            .with_context("resource_kind", kind)
            .with_context("resource_name", name)
    }

    /// Precondition failed — something must be done first.
    pub fn precondition(message: impl Into<String>) -> Self {
        Self::new("PRECONDITION_FAILED", message)
    }

    /// Internal error — something unexpected happened.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("INTERNAL_ERROR", message)
    }

    /// Daemon not reachable.
    pub fn daemon_unreachable() -> Self {
        Self::new(
            "DAEMON_UNREACHABLE",
            "cannot reach the syfrah daemon — is it running?",
        )
        .with_suggestion(
            "Start it with: syfrah fabric init --name <mesh> --region <region> --zone <zone>",
        )
    }

    /// Ambiguous name — multiple resources match.
    pub fn ambiguous(kind: &str, name: &str, matches: Vec<(String, String)>) -> Self {
        let match_list: String = matches
            .iter()
            .map(|(id, ctx)| format!("  {id} ({ctx})"))
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(
            "AMBIGUOUS_NAME",
            format!(
                "multiple {kind}s named '{name}':\n{match_list}\nUse the ID directly or add scope flags to disambiguate."
            ),
        )
        .with_context("resource_kind", kind)
        .with_context("resource_name", name)
    }

    /// Timeout waiting for an operation.
    pub fn timeout(operation: &str, duration_secs: u64) -> Self {
        Self::new(
            "TIMEOUT",
            format!("'{operation}' timed out after {duration_secs}s"),
        )
    }

    /// Rate limited.
    pub fn rate_limited() -> Self {
        Self::new("RATE_LIMITED", "too many requests — try again later")
    }
}

impl fmt::Display for SyfrahError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error: {}", self.message)?;
        if let Some(suggestion) = &self.suggestion {
            write!(f, "\n{suggestion}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SyfrahError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_has_correct_code() {
        let err = SyfrahError::not_found("vpc", "my-vpc");
        assert_eq!(err.code, "RESOURCE_NOT_FOUND");
        assert!(err.message.contains("my-vpc"));
    }

    #[test]
    fn already_exists() {
        let err = SyfrahError::already_exists("org", "acme");
        assert_eq!(err.code, "RESOURCE_ALREADY_EXISTS");
        assert!(err.message.contains("acme"));
    }

    #[test]
    fn with_suggestion() {
        let err = SyfrahError::not_found("vpc", "web").with_suggestion("Run: syfrah vpc list");
        assert_eq!(err.suggestion.as_deref(), Some("Run: syfrah vpc list"));
        let display = format!("{err}");
        assert!(display.contains("Run: syfrah vpc list"));
    }

    #[test]
    fn with_context() {
        let err = SyfrahError::not_found("vm", "web-1")
            .with_context("zone", "fsn1")
            .with_context("hypervisor", "hv-01");
        assert_eq!(err.context.len(), 4); // 2 from not_found + 2 added
    }

    #[test]
    fn display_format() {
        let err = SyfrahError::not_found("vpc", "my-vpc");
        let s = format!("{err}");
        assert!(s.starts_with("Error: "));
        assert!(s.contains("my-vpc"));
    }

    #[test]
    fn display_with_suggestion() {
        let err = SyfrahError::daemon_unreachable();
        let s = format!("{err}");
        assert!(s.contains("cannot reach"));
        assert!(s.contains("syfrah fabric init"));
    }

    #[test]
    fn validation_error() {
        let err = SyfrahError::validation("CIDR must include prefix length");
        assert_eq!(err.code, "VALIDATION_ERROR");
    }

    #[test]
    fn invalid_name() {
        let err = SyfrahError::invalid_name("MY_VPC", "must be lowercase");
        assert_eq!(err.code, "INVALID_NAME");
        assert!(err.message.contains("MY_VPC"));
        assert!(err.message.contains("lowercase"));
    }

    #[test]
    fn conflict() {
        let err = SyfrahError::conflict("subnet", "web", "has active VMs");
        assert_eq!(err.code, "CONFLICT");
        assert!(err.message.contains("active VMs"));
    }

    #[test]
    fn precondition() {
        let err = SyfrahError::precondition("storage not configured for zone fsn1")
            .with_suggestion("Run: syfrah storage configure --zone fsn1 ...");
        assert_eq!(err.code, "PRECONDITION_FAILED");
        assert!(err.suggestion.is_some());
    }

    #[test]
    fn ambiguous() {
        let err = SyfrahError::ambiguous(
            "vpc",
            "web",
            vec![
                ("vpc-01AAA".into(), "org: acme".into()),
                ("vpc-01BBB".into(), "org: other".into()),
            ],
        );
        assert_eq!(err.code, "AMBIGUOUS_NAME");
        assert!(err.message.contains("vpc-01AAA"));
        assert!(err.message.contains("vpc-01BBB"));
        assert!(err.message.contains("disambiguate"));
    }

    #[test]
    fn timeout() {
        let err = SyfrahError::timeout("vm create", 60);
        assert_eq!(err.code, "TIMEOUT");
        assert!(err.message.contains("60s"));
    }

    #[test]
    fn serde_roundtrip() {
        let err = SyfrahError::not_found("vpc", "web")
            .with_suggestion("Run: syfrah vpc list")
            .with_context("zone", "fsn1");
        let json = serde_json::to_string(&err).unwrap();
        let back: SyfrahError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.code, err.code);
        assert_eq!(back.message, err.message);
        assert_eq!(back.suggestion, err.suggestion);
    }

    #[test]
    fn serde_skips_empty_fields() {
        let err = SyfrahError::validation("bad input");
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("suggestion"));
        assert!(!json.contains("context"));
    }

    #[test]
    fn json_output_format() {
        let err = SyfrahError::not_found("vpc", "web").with_suggestion("Run: syfrah vpc list");
        let json = serde_json::to_string_pretty(&err).unwrap();
        // Should look like a proper API error response
        assert!(json.contains("\"code\""));
        assert!(json.contains("\"message\""));
        assert!(json.contains("\"suggestion\""));
    }

    #[test]
    fn into_anyhow() {
        let err = SyfrahError::not_found("vpc", "web");
        // SyfrahError implements std::error::Error, so anyhow::Error::from works
        let anyhow_err = anyhow::Error::new(err);
        let s = format!("{anyhow_err}");
        assert!(s.contains("web"));
    }

    #[test]
    fn daemon_unreachable() {
        let err = SyfrahError::daemon_unreachable();
        assert_eq!(err.code, "DAEMON_UNREACHABLE");
        assert!(err.suggestion.is_some());
    }

    #[test]
    fn not_implemented() {
        let err = SyfrahError::not_implemented("vm resize");
        assert_eq!(err.code, "NOT_IMPLEMENTED");
        assert!(err.message.contains("vm resize"));
    }

    #[test]
    fn rate_limited() {
        let err = SyfrahError::rate_limited();
        assert_eq!(err.code, "RATE_LIMITED");
    }
}
