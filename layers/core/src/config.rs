//! Configuration management for Syfrah.
//!
//! Reads `~/.syfrah/config.toml`. Optional — every setting has a sensible default.
//! Invalid TOML = daemon refuses to start. Unknown keys are silently ignored.
//!
//! # Usage
//!
//! ```
//! use syfrah_core::config::Config;
//!
//! let config = Config::default();
//! assert_eq!(config.daemon.health_check_interval_secs, 60);
//! assert_eq!(config.wireguard.interface_name, "syfrah0");
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::SyfrahError;

/// Root configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub wireguard: WireguardConfig,
    pub peering: PeeringConfig,
    pub storage: StorageConfig,
    pub logging: LoggingConfig,
}

/// Daemon behavior settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Health check interval in seconds.
    pub health_check_interval_secs: u64,
    /// Reconciliation loop interval in seconds.
    pub reconcile_interval_secs: u64,
    /// State persistence interval in seconds.
    pub persist_interval_secs: u64,
    /// Time before a peer is marked unreachable (seconds).
    pub unreachable_timeout_secs: u64,
    /// Maximum number of concurrent API requests.
    pub max_concurrent_requests: u32,
}

/// WireGuard interface settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WireguardConfig {
    /// WireGuard interface name.
    pub interface_name: String,
    /// Persistent keepalive interval in seconds.
    pub keepalive_interval_secs: u64,
    /// WireGuard listen port.
    pub listen_port: u16,
}

/// Peering protocol settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PeeringConfig {
    /// Timeout for join operations in seconds.
    pub join_timeout_secs: u64,
    /// Timeout for key exchange in seconds.
    pub exchange_timeout_secs: u64,
    /// Maximum concurrent incoming connections.
    pub max_concurrent_connections: u32,
    /// Maximum pending join requests.
    pub max_pending_joins: u32,
}

/// Storage backend settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Cache memory limit in MB.
    pub cache_memory_mb: u64,
    /// Cache disk size limit in GB.
    pub cache_disk_gb: u64,
}

/// Logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Log level: trace, debug, info, warn, error.
    pub level: String,
    /// Log format: text, json.
    pub format: String,
    /// Log file path (empty = stderr only).
    pub file: String,
    /// Max log file size in MB before rotation.
    pub max_file_size_mb: u64,
    /// Number of rotated log files to keep.
    pub max_files: u32,
}

// ── Defaults ──────────────────────────────────────────────

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            health_check_interval_secs: 60,
            reconcile_interval_secs: 30,
            persist_interval_secs: 30,
            unreachable_timeout_secs: 300,
            max_concurrent_requests: 100,
        }
    }
}

impl Default for WireguardConfig {
    fn default() -> Self {
        Self {
            interface_name: "syfrah0".to_string(),
            keepalive_interval_secs: 25,
            listen_port: 51820,
        }
    }
}

impl Default for PeeringConfig {
    fn default() -> Self {
        Self {
            join_timeout_secs: 300,
            exchange_timeout_secs: 30,
            max_concurrent_connections: 100,
            max_pending_joins: 100,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            cache_memory_mb: 4096,
            cache_disk_gb: 100,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "text".to_string(),
            file: String::new(),
            max_file_size_mb: 50,
            max_files: 3,
        }
    }
}

// ── Loading ──────────────────────────────────────────────

/// Default config file path.
pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
        .join("config.toml")
}

impl Config {
    /// Load config from `~/.syfrah/config.toml`.
    /// Returns defaults if the file doesn't exist.
    /// Returns error if the file exists but is invalid TOML.
    pub fn load() -> Result<Self, SyfrahError> {
        Self::load_from(&config_path())
    }

    /// Load config from a specific path.
    pub fn load_from(path: &Path) -> Result<Self, SyfrahError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path).map_err(|e| {
            SyfrahError::internal(format!(
                "failed to read config file '{}': {e}",
                path.display()
            ))
        })?;

        Self::parse(&contents)
    }

    /// Parse config from a TOML string.
    pub fn parse(toml_str: &str) -> Result<Self, SyfrahError> {
        toml::from_str(toml_str).map_err(|e| {
            SyfrahError::validation(format!("invalid config.toml: {e}"))
                .with_suggestion("Fix the syntax error and restart the daemon.")
        })
    }

    /// Write the current config to a file.
    pub fn save(&self, path: &Path) -> Result<(), SyfrahError> {
        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| SyfrahError::internal(format!("failed to serialize config: {e}")))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SyfrahError::from)?;
        }

        std::fs::write(path, toml_str).map_err(SyfrahError::from)?;
        Ok(())
    }

    /// Generate a default config file with comments.
    pub fn generate_default() -> String {
        r#"# Syfrah configuration
# All settings are optional — defaults shown below.

[daemon]
health_check_interval_secs = 60
reconcile_interval_secs = 30
persist_interval_secs = 30
unreachable_timeout_secs = 300
max_concurrent_requests = 100

[wireguard]
interface_name = "syfrah0"
keepalive_interval_secs = 25
listen_port = 51820

[peering]
join_timeout_secs = 300
exchange_timeout_secs = 30
max_concurrent_connections = 100
max_pending_joins = 100

[storage]
cache_memory_mb = 4096
cache_disk_gb = 100

[logging]
level = "info"       # trace, debug, info, warn, error
format = "text"      # text, json
file = ""            # empty = stderr only
max_file_size_mb = 50
max_files = 3
"#
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let c = Config::default();
        assert_eq!(c.daemon.health_check_interval_secs, 60);
        assert_eq!(c.daemon.reconcile_interval_secs, 30);
        assert_eq!(c.daemon.unreachable_timeout_secs, 300);
        assert_eq!(c.wireguard.interface_name, "syfrah0");
        assert_eq!(c.wireguard.listen_port, 51820);
        assert_eq!(c.wireguard.keepalive_interval_secs, 25);
        assert_eq!(c.peering.join_timeout_secs, 300);
        assert_eq!(c.peering.max_concurrent_connections, 100);
        assert_eq!(c.storage.cache_memory_mb, 4096);
        assert_eq!(c.logging.level, "info");
        assert_eq!(c.logging.format, "text");
        assert_eq!(c.logging.max_files, 3);
    }

    #[test]
    fn parse_empty_returns_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.daemon.health_check_interval_secs, 60);
    }

    #[test]
    fn parse_partial_config() {
        let c = Config::parse(
            r#"
            [daemon]
            health_check_interval_secs = 120
            "#,
        )
        .unwrap();
        assert_eq!(c.daemon.health_check_interval_secs, 120);
        // Others still default
        assert_eq!(c.daemon.reconcile_interval_secs, 30);
        assert_eq!(c.wireguard.interface_name, "syfrah0");
    }

    #[test]
    fn parse_full_config() {
        let c = Config::parse(&Config::generate_default()).unwrap();
        assert_eq!(c.daemon.health_check_interval_secs, 60);
        assert_eq!(c.wireguard.interface_name, "syfrah0");
        assert_eq!(c.logging.level, "info");
    }

    #[test]
    fn parse_invalid_toml() {
        let result = Config::parse("not [valid toml {{{");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("invalid config.toml"));
        assert!(err.suggestion.is_some());
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = Config::parse(
            r#"
            [daemon]
            health_check_interval_secs = 10
            unknown_key = "ignored"

            [unknown_section]
            whatever = true
            "#,
        )
        .unwrap();
        assert_eq!(c.daemon.health_check_interval_secs, 10);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let c = Config::load_from(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(c.daemon.health_check_interval_secs, 60);
    }

    #[test]
    fn save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut c = Config::default();
        c.daemon.health_check_interval_secs = 999;
        c.wireguard.interface_name = "test0".to_string();
        c.save(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.daemon.health_check_interval_secs, 999);
        assert_eq!(loaded.wireguard.interface_name, "test0");
    }

    #[test]
    fn generate_default_is_valid_toml() {
        let default_str = Config::generate_default();
        let c = Config::parse(&default_str).unwrap();
        assert_eq!(c.daemon.health_check_interval_secs, 60);
    }

    #[test]
    fn config_path_in_syfrah_dir() {
        let p = config_path();
        assert!(p.to_str().unwrap().contains(".syfrah/config.toml"));
    }

    #[test]
    fn serde_roundtrip() {
        let c = Config::default();
        let toml_str = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            back.daemon.health_check_interval_secs,
            c.daemon.health_check_interval_secs
        );
    }

    #[test]
    fn override_single_field() {
        let c = Config::parse("[wireguard]\nlisten_port = 9999\n").unwrap();
        assert_eq!(c.wireguard.listen_port, 9999);
        assert_eq!(c.wireguard.interface_name, "syfrah0"); // other fields default
    }

    #[test]
    fn logging_json_format() {
        let c = Config::parse("[logging]\nformat = \"json\"\n").unwrap();
        assert_eq!(c.logging.format, "json");
    }
}
