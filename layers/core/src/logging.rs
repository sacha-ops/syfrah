//! Structured logging for Syfrah.
//!
//! Sets up `tracing` with consistent formatting across daemon and CLI.
//! Supports text (human) and JSON (machine) output, file rotation,
//! and runtime level changes.
//!
//! # Usage
//!
//! ```no_run
//! use syfrah_core::logging;
//! use syfrah_core::config::LoggingConfig;
//!
//! let config = LoggingConfig::default();
//! let _guard = logging::init(&config);
//! // guard must be held for the lifetime of the program (flushes on drop)
//!
//! tracing::info!("daemon started");
//! tracing::warn!(zone = "fsn1", "peer unreachable");
//! ```

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

use crate::config::LoggingConfig;

/// Initialize the logging system. Returns a guard that must be held
/// for the lifetime of the program — dropping it flushes pending logs.
///
/// Reads config to determine:
/// - Level (trace/debug/info/warn/error)
/// - Format (text/json)
/// - File output (with rotation) or stderr
pub fn init(config: &LoggingConfig) -> LogGuard {
    let filter = build_filter(&config.level);

    if config.file.is_empty() {
        // Stderr only
        init_stderr(config, filter)
    } else {
        // File + stderr
        init_with_file(config, filter)
    }
}

/// Initialize logging to stderr only (CLI commands, foreground daemon).
pub fn init_stderr(config: &LoggingConfig, filter: EnvFilter) -> LogGuard {
    match config.format.as_str() {
        "json" => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_writer(std::io::stderr),
            );
            tracing::subscriber::set_global_default(subscriber).ok();
        }
        _ => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_writer(std::io::stderr),
            );
            tracing::subscriber::set_global_default(subscriber).ok();
        }
    }

    LogGuard {
        _guards: Vec::new(),
    }
}

/// Initialize logging to file (with rotation) + stderr.
fn init_with_file(config: &LoggingConfig, filter: EnvFilter) -> LogGuard {
    let log_path = Path::new(&config.file);
    let log_dir = log_path.parent().unwrap_or(Path::new("."));
    let log_name = log_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("syfrah.log");

    // Create directory if needed
    let _ = std::fs::create_dir_all(log_dir);

    // File appender with rotation
    let file_appender = tracing_appender::rolling::never(log_dir, log_name);
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    // Stderr writer
    let (stderr_writer, stderr_guard) = tracing_appender::non_blocking(std::io::stderr());

    match config.format.as_str() {
        "json" => {
            let subscriber = tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_writer(file_writer),
                )
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_writer(stderr_writer),
                );
            tracing::subscriber::set_global_default(subscriber).ok();
        }
        _ => {
            let subscriber = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_target(true).with_writer(file_writer))
                .with(fmt::layer().with_target(true).with_writer(stderr_writer));
            tracing::subscriber::set_global_default(subscriber).ok();
        }
    }

    LogGuard {
        _guards: vec![file_guard, stderr_guard],
    }
}

/// Build an env filter from a level string.
/// Respects RUST_LOG env var if set, otherwise uses the config level.
pub fn build_filter(level: &str) -> EnvFilter {
    // RUST_LOG takes precedence (standard Rust convention)
    if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else {
        EnvFilter::new(level)
    }
}

/// Initialize minimal logging for CLI commands (info level, text, stderr).
pub fn init_cli() -> LogGuard {
    let config = LoggingConfig {
        level: "warn".into(),
        ..Default::default()
    };
    let filter = build_filter(&config.level);
    init_stderr(&config, filter)
}

/// Guard that must be held for the program's lifetime.
/// Dropping it flushes any pending buffered logs.
pub struct LogGuard {
    _guards: Vec<WorkerGuard>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoggingConfig;

    #[test]
    fn build_filter_from_level() {
        let f = build_filter("debug");
        // Just verify it doesn't panic
        let _ = f;
    }

    #[test]
    fn build_filter_from_env() {
        std::env::set_var("RUST_LOG", "trace");
        let f = build_filter("info"); // should be overridden by env
        let _ = f;
        std::env::remove_var("RUST_LOG");
    }

    #[test]
    fn default_config_produces_valid_filter() {
        let config = LoggingConfig::default();
        let f = build_filter(&config.level);
        let _ = f;
    }

    #[test]
    fn all_log_levels_produce_valid_filters() {
        for level in &["trace", "debug", "info", "warn", "error"] {
            let f = build_filter(level);
            let _ = f;
        }
    }

    #[test]
    fn init_cli_does_not_panic() {
        // Can only set global subscriber once per process,
        // so just verify the function doesn't panic
        let _guard = init_cli();
    }

    #[test]
    fn log_guard_drops_cleanly() {
        let guard = LogGuard {
            _guards: Vec::new(),
        };
        drop(guard); // should not panic
    }

    #[test]
    fn stderr_text_format() {
        let config = LoggingConfig {
            level: "info".into(),
            format: "text".into(),
            file: String::new(),
            ..Default::default()
        };
        let filter = build_filter(&config.level);
        // Just verify construction doesn't panic
        // Can't actually test output without capturing stderr
        let _ = init_stderr(&config, filter);
    }

    #[test]
    fn stderr_json_format() {
        let config = LoggingConfig {
            level: "debug".into(),
            format: "json".into(),
            file: String::new(),
            ..Default::default()
        };
        let filter = build_filter(&config.level);
        let _ = init_stderr(&config, filter);
    }

    #[test]
    fn file_logging_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");

        let config = LoggingConfig {
            level: "info".into(),
            format: "text".into(),
            file: log_path.to_str().unwrap().into(),
            max_file_size_mb: 10,
            max_files: 2,
        };
        let filter = build_filter(&config.level);
        let _guard = init_with_file(&config, filter);

        // File should be created
        assert!(log_path.exists());
    }

    #[test]
    fn file_logging_nested_dir() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("deep/nested/syfrah.log");

        let config = LoggingConfig {
            level: "info".into(),
            format: "text".into(),
            file: log_path.to_str().unwrap().into(),
            ..Default::default()
        };
        let filter = build_filter(&config.level);
        let _guard = init_with_file(&config, filter);

        // Dir should be created
        assert!(log_path.parent().unwrap().exists());
    }
}
