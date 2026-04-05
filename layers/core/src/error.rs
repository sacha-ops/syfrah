//! Unified error types for Syfrah.
//!
//! Every layer uses [`SyfrahError`] so that error messages are consistent,
//! actionable, and machine-parseable across CLI and API.
//!
//! # Design
//!
//! - **Code** (`ErrorCode`): typed enum — compiler catches typos
//! - **Message**: human-readable explanation
//! - **Suggestion**: actionable next step ("Run: syfrah vpc list")
//! - **Context**: structured metadata for debugging
//! - **HTTP status**: each code maps to an HTTP status for the API
//! - **Retryable**: callers know whether to retry
//! - **Source**: wraps the original error (io, serde, etc.)
//!
//! ```
//! use syfrah_core::error::{SyfrahError, ErrorCode};
//!
//! let err = SyfrahError::not_found("vpc", "my-vpc")
//!     .with_suggestion("List available VPCs with: syfrah vpc list");
//! assert_eq!(err.code, ErrorCode::ResourceNotFound);
//! assert_eq!(err.code.http_status(), 404);
//! assert!(!err.code.is_retryable());
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;

// ═══════════════════════════════════════════════════
// ErrorCode — typed enum, not a String
// ═══════════════════════════════════════════════════

/// Machine-readable error code. Every possible error has exactly one code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // ── Client errors (4xx) ──
    ResourceNotFound,
    ResourceAlreadyExists,
    ValidationError,
    InvalidName,
    PermissionDenied,
    Conflict,
    PreconditionFailed,
    AmbiguousName,
    RateLimited,

    // ── Server errors (5xx) ──
    InternalError,
    NotImplemented,
    DaemonUnreachable,
    Timeout,

    // ── Network/transient ──
    NetworkError,
    StorageError,
}

impl ErrorCode {
    /// Map to HTTP status code for API responses.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::ResourceNotFound => 404,
            Self::ResourceAlreadyExists => 409,
            Self::ValidationError => 400,
            Self::InvalidName => 400,
            Self::PermissionDenied => 403,
            Self::Conflict => 409,
            Self::PreconditionFailed => 412,
            Self::AmbiguousName => 400,
            Self::RateLimited => 429,
            Self::InternalError => 500,
            Self::NotImplemented => 501,
            Self::DaemonUnreachable => 503,
            Self::Timeout => 504,
            Self::NetworkError => 502,
            Self::StorageError => 500,
        }
    }

    /// Whether the caller should retry this error.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::RateLimited | Self::NetworkError | Self::DaemonUnreachable
        )
    }

    /// Whether this is a client error (4xx).
    pub fn is_client_error(&self) -> bool {
        self.http_status() >= 400 && self.http_status() < 500
    }

    /// Whether this is a server error (5xx).
    pub fn is_server_error(&self) -> bool {
        self.http_status() >= 500
    }

    /// The string code for serialization (e.g., "RESOURCE_NOT_FOUND").
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ResourceNotFound => "RESOURCE_NOT_FOUND",
            Self::ResourceAlreadyExists => "RESOURCE_ALREADY_EXISTS",
            Self::ValidationError => "VALIDATION_ERROR",
            Self::InvalidName => "INVALID_NAME",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::Conflict => "CONFLICT",
            Self::PreconditionFailed => "PRECONDITION_FAILED",
            Self::AmbiguousName => "AMBIGUOUS_NAME",
            Self::RateLimited => "RATE_LIMITED",
            Self::InternalError => "INTERNAL_ERROR",
            Self::NotImplemented => "NOT_IMPLEMENTED",
            Self::DaemonUnreachable => "DAEMON_UNREACHABLE",
            Self::Timeout => "TIMEOUT",
            Self::NetworkError => "NETWORK_ERROR",
            Self::StorageError => "STORAGE_ERROR",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ═══════════════════════════════════════════════════
// SyfrahError
// ═══════════════════════════════════════════════════

/// The unified error type for all of Syfrah.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyfrahError {
    /// Typed error code.
    pub code: ErrorCode,
    /// Human-readable error message.
    pub message: String,
    /// Actionable suggestion for the operator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Structured context for debugging.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub context: Vec<(String, String)>,
    /// Original error message (not serialized — for logging only).
    #[serde(skip)]
    pub source_message: Option<String>,
}

impl SyfrahError {
    /// Create a new error.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: None,
            context: Vec::new(),
            source_message: None,
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

    /// Attach the source error message.
    pub fn with_source(mut self, source: &dyn std::error::Error) -> Self {
        self.source_message = Some(source.to_string());
        self
    }

    /// HTTP status code for this error.
    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }

    /// Whether the caller should retry.
    pub fn is_retryable(&self) -> bool {
        self.code.is_retryable()
    }

    /// Format for CLI output (human-friendly).
    pub fn format_cli(&self) -> String {
        let mut out = format!("Error: {}", self.message);
        if let Some(suggestion) = &self.suggestion {
            out.push_str(&format!("\n{suggestion}"));
        }
        out
    }

    /// Format as JSON (for --json mode or API responses).
    pub fn format_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|_| format!("{{\"error\": \"{}\"}}", self.message))
    }

    // ── Common constructors ──────────────────────────────

    pub fn not_found(kind: &str, name: &str) -> Self {
        Self::new(
            ErrorCode::ResourceNotFound,
            format!("{kind} '{name}' not found"),
        )
        .with_context("resource_kind", kind)
        .with_context("resource_name", name)
    }

    pub fn already_exists(kind: &str, name: &str) -> Self {
        Self::new(
            ErrorCode::ResourceAlreadyExists,
            format!("{kind} '{name}' already exists"),
        )
        .with_context("resource_kind", kind)
        .with_context("resource_name", name)
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationError, message)
    }

    pub fn invalid_name(name: &str, reason: &str) -> Self {
        Self::new(
            ErrorCode::InvalidName,
            format!("invalid name '{name}': {reason}"),
        )
        .with_context("name", name)
    }

    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }

    pub fn not_implemented(operation: &str) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("'{operation}' is not yet implemented"),
        )
    }

    pub fn conflict(kind: &str, name: &str, reason: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::Conflict,
            format!("{kind} '{name}': {}", reason.into()),
        )
        .with_context("resource_kind", kind)
        .with_context("resource_name", name)
    }

    pub fn precondition(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PreconditionFailed, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, message)
    }

    pub fn daemon_unreachable() -> Self {
        Self::new(
            ErrorCode::DaemonUnreachable,
            "cannot reach the syfrah daemon — is it running?",
        )
        .with_suggestion(
            "Start it with: syfrah fabric init --name <mesh> --region <region> --zone <zone>",
        )
    }

    pub fn ambiguous(kind: &str, name: &str, matches: Vec<(String, String)>) -> Self {
        let match_list: String = matches
            .iter()
            .map(|(id, ctx)| format!("  {id} ({ctx})"))
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(
            ErrorCode::AmbiguousName,
            format!(
                "multiple {kind}s named '{name}':\n{match_list}\n\
                 Use the ID directly or add scope flags to disambiguate."
            ),
        )
        .with_context("resource_kind", kind)
        .with_context("resource_name", name)
    }

    pub fn timeout(operation: &str, duration_secs: u64) -> Self {
        Self::new(
            ErrorCode::Timeout,
            format!("'{operation}' timed out after {duration_secs}s"),
        )
    }

    pub fn rate_limited() -> Self {
        Self::new(
            ErrorCode::RateLimited,
            "too many requests — try again later",
        )
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NetworkError, message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::StorageError, message)
    }
}

// ── Display: used by CLI when not in --json mode ──

impl fmt::Display for SyfrahError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error: {}", self.message)?;
        if let Some(suggestion) = &self.suggestion {
            write!(f, "\n{suggestion}")?;
        }
        if let Some(source) = &self.source_message {
            write!(f, "\nCaused by: {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SyfrahError {}

// ── From conversions for common error types ──

impl From<std::io::Error> for SyfrahError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::new(ErrorCode::ResourceNotFound, e.to_string()),
            std::io::ErrorKind::PermissionDenied => {
                Self::new(ErrorCode::PermissionDenied, e.to_string())
            }
            std::io::ErrorKind::TimedOut => Self::new(ErrorCode::Timeout, e.to_string()),
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted => {
                Self::new(ErrorCode::NetworkError, e.to_string())
            }
            _ => Self::new(ErrorCode::InternalError, e.to_string()),
        }
        .with_source(&e)
    }
}

impl From<serde_json::Error> for SyfrahError {
    fn from(e: serde_json::Error) -> Self {
        Self::new(ErrorCode::InternalError, format!("JSON error: {e}")).with_source(&e)
    }
}

// ── Result type alias ──

/// Convenience alias for `Result<T, SyfrahError>`.
pub type SyfrahResult<T> = Result<T, SyfrahError>;

#[cfg(test)]
mod tests {
    use super::*;

    // ── ErrorCode tests ──

    #[test]
    fn error_code_http_status() {
        assert_eq!(ErrorCode::ResourceNotFound.http_status(), 404);
        assert_eq!(ErrorCode::ResourceAlreadyExists.http_status(), 409);
        assert_eq!(ErrorCode::ValidationError.http_status(), 400);
        assert_eq!(ErrorCode::PermissionDenied.http_status(), 403);
        assert_eq!(ErrorCode::RateLimited.http_status(), 429);
        assert_eq!(ErrorCode::InternalError.http_status(), 500);
        assert_eq!(ErrorCode::NotImplemented.http_status(), 501);
        assert_eq!(ErrorCode::DaemonUnreachable.http_status(), 503);
        assert_eq!(ErrorCode::Timeout.http_status(), 504);
    }

    #[test]
    fn error_code_retryable() {
        assert!(ErrorCode::Timeout.is_retryable());
        assert!(ErrorCode::RateLimited.is_retryable());
        assert!(ErrorCode::NetworkError.is_retryable());
        assert!(ErrorCode::DaemonUnreachable.is_retryable());
        assert!(!ErrorCode::ResourceNotFound.is_retryable());
        assert!(!ErrorCode::ValidationError.is_retryable());
        assert!(!ErrorCode::InternalError.is_retryable());
    }

    #[test]
    fn error_code_client_vs_server() {
        assert!(ErrorCode::ResourceNotFound.is_client_error());
        assert!(!ErrorCode::ResourceNotFound.is_server_error());
        assert!(ErrorCode::InternalError.is_server_error());
        assert!(!ErrorCode::InternalError.is_client_error());
    }

    #[test]
    fn error_code_as_str() {
        assert_eq!(ErrorCode::ResourceNotFound.as_str(), "RESOURCE_NOT_FOUND");
        assert_eq!(ErrorCode::AmbiguousName.as_str(), "AMBIGUOUS_NAME");
    }

    #[test]
    fn error_code_display() {
        assert_eq!(
            format!("{}", ErrorCode::ResourceNotFound),
            "RESOURCE_NOT_FOUND"
        );
    }

    // ── SyfrahError constructor tests ──

    #[test]
    fn not_found() {
        let err = SyfrahError::not_found("vpc", "my-vpc");
        assert_eq!(err.code, ErrorCode::ResourceNotFound);
        assert!(err.message.contains("my-vpc"));
        assert_eq!(err.http_status(), 404);
        assert!(!err.is_retryable());
    }

    #[test]
    fn already_exists() {
        let err = SyfrahError::already_exists("org", "acme");
        assert_eq!(err.code, ErrorCode::ResourceAlreadyExists);
        assert!(err.message.contains("acme"));
    }

    #[test]
    fn with_suggestion() {
        let err = SyfrahError::not_found("vpc", "web").with_suggestion("Run: syfrah vpc list");
        assert_eq!(err.suggestion.as_deref(), Some("Run: syfrah vpc list"));
    }

    #[test]
    fn with_context() {
        let err = SyfrahError::not_found("vm", "web-1").with_context("zone", "fsn1");
        // 2 from not_found + 1 added
        assert_eq!(err.context.len(), 3);
    }

    #[test]
    fn with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = SyfrahError::internal("something failed").with_source(&io_err);
        assert_eq!(err.source_message.as_deref(), Some("file missing"));
    }

    #[test]
    fn conflict() {
        let err = SyfrahError::conflict("subnet", "web", "has active VMs");
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(err.http_status(), 409);
    }

    #[test]
    fn precondition() {
        let err = SyfrahError::precondition("storage not configured")
            .with_suggestion("Run: syfrah storage configure ...");
        assert_eq!(err.code, ErrorCode::PreconditionFailed);
        assert_eq!(err.http_status(), 412);
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
        assert_eq!(err.code, ErrorCode::AmbiguousName);
        assert!(err.message.contains("vpc-01AAA"));
        assert!(err.message.contains("disambiguate"));
    }

    #[test]
    fn timeout() {
        let err = SyfrahError::timeout("vm create", 60);
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(err.is_retryable());
    }

    #[test]
    fn rate_limited() {
        let err = SyfrahError::rate_limited();
        assert!(err.is_retryable());
        assert_eq!(err.http_status(), 429);
    }

    #[test]
    fn daemon_unreachable() {
        let err = SyfrahError::daemon_unreachable();
        assert!(err.is_retryable());
        assert!(err.suggestion.is_some());
    }

    #[test]
    fn network_and_storage() {
        let err = SyfrahError::network("connection refused");
        assert_eq!(err.code, ErrorCode::NetworkError);
        assert!(err.is_retryable());

        let err = SyfrahError::storage("disk full");
        assert_eq!(err.code, ErrorCode::StorageError);
        assert!(!err.is_retryable());
    }

    // ── Display tests ──

    #[test]
    fn display_basic() {
        let err = SyfrahError::not_found("vpc", "web");
        let s = format!("{err}");
        assert!(s.starts_with("Error: "));
        assert!(s.contains("web"));
    }

    #[test]
    fn display_with_suggestion() {
        let err = SyfrahError::daemon_unreachable();
        let s = format!("{err}");
        assert!(s.contains("cannot reach"));
        assert!(s.contains("syfrah fabric init"));
    }

    #[test]
    fn display_with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err = SyfrahError::internal("read failed").with_source(&io_err);
        let s = format!("{err}");
        assert!(s.contains("Caused by: no such file"));
    }

    // ── Formatting tests ──

    #[test]
    fn format_cli() {
        let err = SyfrahError::not_found("vpc", "web").with_suggestion("Run: syfrah vpc list");
        let cli = err.format_cli();
        assert!(cli.starts_with("Error:"));
        assert!(cli.contains("Run: syfrah vpc list"));
    }

    #[test]
    fn format_json() {
        let err = SyfrahError::not_found("vpc", "web");
        let json = err.format_json();
        assert!(json.contains("RESOURCE_NOT_FOUND"));
        assert!(json.contains("web"));
    }

    // ── Serde tests ──

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
    fn serde_code_is_screaming_snake() {
        let err = SyfrahError::not_found("vpc", "web");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"RESOURCE_NOT_FOUND\""));
    }

    // ── From conversions ──

    #[test]
    fn from_io_error_not_found() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err: SyfrahError = io_err.into();
        assert_eq!(err.code, ErrorCode::ResourceNotFound);
        assert!(err.source_message.is_some());
    }

    #[test]
    fn from_io_error_permission() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err: SyfrahError = io_err.into();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }

    #[test]
    fn from_io_error_timeout() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let err: SyfrahError = io_err.into();
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(err.is_retryable());
    }

    #[test]
    fn from_io_error_connection() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let err: SyfrahError = io_err.into();
        assert_eq!(err.code, ErrorCode::NetworkError);
    }

    #[test]
    fn from_serde_error() {
        let serde_err = serde_json::from_str::<String>("not json").unwrap_err();
        let err: SyfrahError = serde_err.into();
        assert_eq!(err.code, ErrorCode::InternalError);
        assert!(err.message.contains("JSON"));
    }

    // ── Result alias ──

    #[test]
    fn result_alias() {
        fn example() -> SyfrahResult<()> {
            Err(SyfrahError::not_found("vpc", "web"))
        }
        assert!(example().is_err());
    }

    // ── Into anyhow ──

    #[test]
    fn into_anyhow() {
        let err = SyfrahError::not_found("vpc", "web");
        let anyhow_err = anyhow::Error::new(err);
        let s = format!("{anyhow_err}");
        assert!(s.contains("web"));
    }
}
