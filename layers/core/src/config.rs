//! Configuration management for Syfrah.
//!
//! Reads `~/.syfrah/config.toml`, overridden by env vars (`SYFRAH_*`),
//! overridden by CLI flags. Invalid values are caught at validation, not at runtime.
//!
//! # Priority (highest wins)
//!
//! 1. CLI flags (`--log-level debug`)
//! 2. Environment variables (`SYFRAH_LOG_LEVEL=debug`)
//! 3. Config file (`~/.syfrah/config.toml`)
//! 4. Defaults
//!
//! ```
//! use syfrah_core::config::Config;
//!
//! let config = Config::default();
//! assert_eq!(config.daemon.health_check_interval, "60s");
//! assert_eq!(config.wireguard.interface_name, "syfrah0");
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::SyfrahError;
use crate::validate;

// ═══════════════════════════════════════════════════
// Config structs — all durations are human-readable strings
// ═══════════════════════════════════════════════════

/// Root configuration. Version field for future schema migrations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Config schema version (for future migrations).
    pub config_version: u32,
    pub daemon: DaemonConfig,
    pub wireguard: WireguardConfig,
    pub peering: PeeringConfig,
    pub storage: StorageConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Health check interval (e.g., "60s", "1m").
    pub health_check_interval: String,
    /// Reconciliation loop interval.
    pub reconcile_interval: String,
    /// State persistence interval.
    pub persist_interval: String,
    /// Time before a peer is marked unreachable.
    pub unreachable_timeout: String,
    /// Maximum concurrent API requests.
    pub max_concurrent_requests: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WireguardConfig {
    /// WireGuard interface name.
    pub interface_name: String,
    /// Persistent keepalive interval.
    pub keepalive_interval: String,
    /// WireGuard listen port.
    pub listen_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PeeringConfig {
    /// Join operation timeout.
    pub join_timeout: String,
    /// Key exchange timeout.
    pub exchange_timeout: String,
    /// Max concurrent incoming connections.
    pub max_concurrent_connections: u32,
    /// Max pending join requests.
    pub max_pending_joins: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Cache memory limit in MB.
    pub cache_memory_mb: u64,
    /// Cache disk size limit in GB.
    pub cache_disk_gb: u64,
}

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

// ── Defaults ──

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            health_check_interval: "60s".into(),
            reconcile_interval: "30s".into(),
            persist_interval: "30s".into(),
            unreachable_timeout: "5m".into(),
            max_concurrent_requests: 100,
        }
    }
}

impl Default for WireguardConfig {
    fn default() -> Self {
        Self {
            interface_name: "syfrah0".into(),
            keepalive_interval: "25s".into(),
            listen_port: 51820,
        }
    }
}

impl Default for PeeringConfig {
    fn default() -> Self {
        Self {
            join_timeout: "5m".into(),
            exchange_timeout: "30s".into(),
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
            level: "info".into(),
            format: "text".into(),
            file: String::new(),
            max_file_size_mb: 50,
            max_files: 3,
        }
    }
}

// ═══════════════════════════════════════════════════
// Env var mapping
// ═══════════════════════════════════════════════════

const ENV_MAP: &[(&str, &str)] = &[
    ("SYFRAH_LOG_LEVEL", "logging.level"),
    ("SYFRAH_LOG_FORMAT", "logging.format"),
    ("SYFRAH_LOG_FILE", "logging.file"),
    ("SYFRAH_WG_INTERFACE", "wireguard.interface_name"),
    ("SYFRAH_WG_PORT", "wireguard.listen_port"),
    ("SYFRAH_HEALTH_INTERVAL", "daemon.health_check_interval"),
    ("SYFRAH_RECONCILE_INTERVAL", "daemon.reconcile_interval"),
    ("SYFRAH_UNREACHABLE_TIMEOUT", "daemon.unreachable_timeout"),
    ("SYFRAH_CACHE_MEMORY_MB", "storage.cache_memory_mb"),
    ("SYFRAH_CACHE_DISK_GB", "storage.cache_disk_gb"),
];

// ═══════════════════════════════════════════════════
// Validation
// ═══════════════════════════════════════════════════

const VALID_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
const VALID_LOG_FORMATS: &[&str] = &["text", "json"];

impl Config {
    /// Validate all config values. Called after parse + env merge.
    pub fn validate(&self) -> Result<Vec<String>, SyfrahError> {
        let mut warnings = Vec::new();

        // Daemon durations
        validate::duration(&self.daemon.health_check_interval).map_err(|_| {
            SyfrahError::validation(format!(
                "daemon.health_check_interval '{}' is invalid (use e.g., 60s, 1m, 5m)",
                self.daemon.health_check_interval
            ))
        })?;
        validate::duration(&self.daemon.reconcile_interval).map_err(|_| {
            SyfrahError::validation(format!(
                "daemon.reconcile_interval '{}' is invalid",
                self.daemon.reconcile_interval
            ))
        })?;
        validate::duration(&self.daemon.persist_interval).map_err(|_| {
            SyfrahError::validation(format!(
                "daemon.persist_interval '{}' is invalid",
                self.daemon.persist_interval
            ))
        })?;
        validate::duration(&self.daemon.unreachable_timeout).map_err(|_| {
            SyfrahError::validation(format!(
                "daemon.unreachable_timeout '{}' is invalid",
                self.daemon.unreachable_timeout
            ))
        })?;

        // WireGuard
        validate::duration(&self.wireguard.keepalive_interval).map_err(|_| {
            SyfrahError::validation(format!(
                "wireguard.keepalive_interval '{}' is invalid",
                self.wireguard.keepalive_interval
            ))
        })?;
        if self.wireguard.listen_port == 0 {
            return Err(SyfrahError::validation("wireguard.listen_port cannot be 0"));
        }

        // Peering
        validate::duration(&self.peering.join_timeout).map_err(|_| {
            SyfrahError::validation(format!(
                "peering.join_timeout '{}' is invalid",
                self.peering.join_timeout
            ))
        })?;
        validate::duration(&self.peering.exchange_timeout).map_err(|_| {
            SyfrahError::validation(format!(
                "peering.exchange_timeout '{}' is invalid",
                self.peering.exchange_timeout
            ))
        })?;

        // Logging
        if !VALID_LOG_LEVELS.contains(&self.logging.level.as_str()) {
            return Err(SyfrahError::validation(format!(
                "logging.level '{}' is invalid. Must be one of: {}",
                self.logging.level,
                VALID_LOG_LEVELS.join(", ")
            )));
        }
        if !VALID_LOG_FORMATS.contains(&self.logging.format.as_str()) {
            return Err(SyfrahError::validation(format!(
                "logging.format '{}' is invalid. Must be one of: {}",
                self.logging.format,
                VALID_LOG_FORMATS.join(", ")
            )));
        }

        // Cross-field warnings
        if self.storage.cache_memory_mb == 0 && self.storage.cache_disk_gb == 0 {
            warnings.push(
                "storage: both cache_memory_mb and cache_disk_gb are 0 — no caching at all".into(),
            );
        }

        Ok(warnings)
    }

    /// Parse a duration field to seconds.
    pub fn duration_secs(field: &str) -> Result<u64, SyfrahError> {
        validate::duration(field)
    }
}

// ═══════════════════════════════════════════════════
// Loading — file → env → validate
// ═══════════════════════════════════════════════════

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
        .join("config.toml")
}

impl Config {
    /// Load config: file → env vars → validate.
    pub fn load() -> Result<Self, SyfrahError> {
        Self::load_from(&config_path())
    }

    /// Load from a specific path, then apply env overrides.
    pub fn load_from(path: &Path) -> Result<Self, SyfrahError> {
        let mut config = if path.exists() {
            let contents = std::fs::read_to_string(path).map_err(|e| {
                SyfrahError::internal(format!("failed to read '{}': {e}", path.display()))
            })?;
            Self::parse(&contents)?
        } else {
            Self::default()
        };

        config.apply_env_overrides();
        config.validate()?;
        Ok(config)
    }

    /// Parse from TOML string (no env, no validation).
    pub fn parse(toml_str: &str) -> Result<Self, SyfrahError> {
        toml::from_str(toml_str).map_err(|e| {
            SyfrahError::validation(format!("invalid config.toml: {e}"))
                .with_suggestion("Fix the syntax error and restart the daemon.")
        })
    }

    /// Apply environment variable overrides.
    pub fn apply_env_overrides(&mut self) {
        for (env_key, config_path) in ENV_MAP {
            if let Ok(val) = std::env::var(env_key) {
                self.set_field(config_path, &val);
            }
        }
    }

    /// Apply CLI overrides (key=value pairs).
    pub fn apply_overrides(&mut self, overrides: &HashMap<String, String>) {
        for (key, val) in overrides {
            self.set_field(key, val);
        }
    }

    fn set_field(&mut self, path: &str, value: &str) {
        match path {
            "logging.level" => self.logging.level = value.into(),
            "logging.format" => self.logging.format = value.into(),
            "logging.file" => self.logging.file = value.into(),
            "wireguard.interface_name" => self.wireguard.interface_name = value.into(),
            "wireguard.listen_port" => {
                if let Ok(p) = value.parse() {
                    self.wireguard.listen_port = p;
                }
            }
            "daemon.health_check_interval" => self.daemon.health_check_interval = value.into(),
            "daemon.reconcile_interval" => self.daemon.reconcile_interval = value.into(),
            "daemon.unreachable_timeout" => self.daemon.unreachable_timeout = value.into(),
            "storage.cache_memory_mb" => {
                if let Ok(v) = value.parse() {
                    self.storage.cache_memory_mb = v;
                }
            }
            "storage.cache_disk_gb" => {
                if let Ok(v) = value.parse() {
                    self.storage.cache_disk_gb = v;
                }
            }
            _ => {} // unknown path, silently ignore
        }
    }

    /// Save config to file (secrets redacted).
    pub fn save(&self, path: &Path) -> Result<(), SyfrahError> {
        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| SyfrahError::internal(format!("serialize config: {e}")))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SyfrahError::from)?;
        }
        std::fs::write(path, toml_str).map_err(SyfrahError::from)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    /// Generate a default config with comments.
    pub fn generate_default() -> String {
        r#"# Syfrah configuration
# All settings are optional — defaults shown below.
# Durations: use s (seconds), m (minutes), h (hours), d (days).
# Env vars override config file (e.g., SYFRAH_LOG_LEVEL=debug).

# config_version = 1

[daemon]
health_check_interval = "60s"
reconcile_interval = "30s"
persist_interval = "30s"
unreachable_timeout = "5m"
max_concurrent_requests = 100

[wireguard]
interface_name = "syfrah0"
keepalive_interval = "25s"
listen_port = 51820

[peering]
join_timeout = "5m"
exchange_timeout = "30s"
max_concurrent_connections = 100
max_pending_joins = 100

[storage]
cache_memory_mb = 4096
cache_disk_gb = 100

[logging]
level = "info"        # trace, debug, info, warn, error
format = "text"       # text, json
file = ""             # empty = stderr only
max_file_size_mb = 50
max_files = 3
"#
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Defaults ──

    #[test]
    fn defaults_are_sensible() {
        let c = Config::default();
        assert_eq!(c.daemon.health_check_interval, "60s");
        assert_eq!(c.daemon.reconcile_interval, "30s");
        assert_eq!(c.daemon.unreachable_timeout, "5m");
        assert_eq!(c.wireguard.interface_name, "syfrah0");
        assert_eq!(c.wireguard.listen_port, 51820);
        assert_eq!(c.wireguard.keepalive_interval, "25s");
        assert_eq!(c.peering.join_timeout, "5m");
        assert_eq!(c.storage.cache_memory_mb, 4096);
        assert_eq!(c.logging.level, "info");
        assert_eq!(c.logging.format, "text");
    }

    #[test]
    fn defaults_validate_ok() {
        let c = Config::default();
        let warnings = c.validate().unwrap();
        assert!(warnings.is_empty());
    }

    // ── Parsing ──

    #[test]
    fn parse_empty() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.daemon.health_check_interval, "60s");
    }

    #[test]
    fn parse_partial() {
        let c = Config::parse("[daemon]\nhealth_check_interval = \"2m\"\n").unwrap();
        assert_eq!(c.daemon.health_check_interval, "2m");
        assert_eq!(c.wireguard.interface_name, "syfrah0");
    }

    #[test]
    fn parse_full() {
        let c = Config::parse(&Config::generate_default()).unwrap();
        assert_eq!(c.daemon.health_check_interval, "60s");
        assert_eq!(c.logging.level, "info");
    }

    #[test]
    fn parse_invalid_toml() {
        let r = Config::parse("not [valid {{{");
        assert!(r.is_err());
        assert!(r.unwrap_err().suggestion.is_some());
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = Config::parse("[daemon]\nunknown = true\n[nope]\nx = 1\n").unwrap();
        assert_eq!(c.daemon.health_check_interval, "60s");
    }

    // ── 1. Validation ──

    #[test]
    fn validate_bad_log_level() {
        let mut c = Config::default();
        c.logging.level = "banana".into();
        let r = c.validate();
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("banana"));
    }

    #[test]
    fn validate_bad_log_format() {
        let mut c = Config::default();
        c.logging.format = "xml".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_bad_duration() {
        let mut c = Config::default();
        c.daemon.health_check_interval = "banana".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_port_zero() {
        let mut c = Config::default();
        c.wireguard.listen_port = 0;
        assert!(c.validate().is_err());
    }

    // ── 5. Cross-field warnings ──

    #[test]
    fn validate_no_cache_warning() {
        let mut c = Config::default();
        c.storage.cache_memory_mb = 0;
        c.storage.cache_disk_gb = 0;
        let warnings = c.validate().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no caching"));
    }

    // ── 3. Env var overrides ──

    #[test]
    fn env_override_log_level() {
        let mut c = Config::default();
        std::env::set_var("SYFRAH_LOG_LEVEL", "debug");
        c.apply_env_overrides();
        assert_eq!(c.logging.level, "debug");
        std::env::remove_var("SYFRAH_LOG_LEVEL");
    }

    #[test]
    fn env_override_port() {
        let mut c = Config::default();
        std::env::set_var("SYFRAH_WG_PORT", "9999");
        c.apply_env_overrides();
        assert_eq!(c.wireguard.listen_port, 9999);
        std::env::remove_var("SYFRAH_WG_PORT");
    }

    // ── 4. CLI overrides ──

    #[test]
    fn cli_overrides() {
        let mut c = Config::default();
        let mut overrides = HashMap::new();
        overrides.insert("logging.level".into(), "debug".into());
        overrides.insert("wireguard.listen_port".into(), "12345".into());
        overrides.insert("daemon.health_check_interval".into(), "2m".into());
        c.apply_overrides(&overrides);

        assert_eq!(c.logging.level, "debug");
        assert_eq!(c.wireguard.listen_port, 12345);
        assert_eq!(c.daemon.health_check_interval, "2m");
    }

    #[test]
    fn cli_unknown_override_ignored() {
        let mut c = Config::default();
        let mut overrides = HashMap::new();
        overrides.insert("nonexistent.field".into(), "value".into());
        c.apply_overrides(&overrides); // no panic
    }

    // ── 8. Duration parsing ──

    #[test]
    fn duration_secs_parses() {
        assert_eq!(Config::duration_secs("60s").unwrap(), 60);
        assert_eq!(Config::duration_secs("5m").unwrap(), 300);
        assert_eq!(Config::duration_secs("2h").unwrap(), 7200);
    }

    // ── 6. Schema version ──

    #[test]
    fn schema_version_default_zero() {
        let c = Config::default();
        assert_eq!(c.config_version, 0);
    }

    #[test]
    fn schema_version_round_trips() {
        let c = Config::parse("config_version = 2\n").unwrap();
        assert_eq!(c.config_version, 2);
    }

    // ── 7. Save with permissions ──

    #[test]
    fn save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut c = Config::default();
        c.daemon.health_check_interval = "2m".into();
        c.wireguard.interface_name = "test0".into();
        c.save(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.daemon.health_check_interval, "2m");
        assert_eq!(loaded.wireguard.interface_name, "test0");
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/nested/config.toml");
        Config::default().save(&path).unwrap();
        assert!(path.exists());
    }

    // ── Load missing ──

    #[test]
    fn load_missing_returns_defaults() {
        let c = Config::load_from(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(c.daemon.health_check_interval, "60s");
    }

    // ── Generate default ──

    #[test]
    fn generate_default_is_valid() {
        let c = Config::parse(&Config::generate_default()).unwrap();
        c.validate().unwrap();
    }

    // ── Path ──

    #[test]
    fn config_path_in_syfrah_dir() {
        let p = config_path();
        assert!(p.to_str().unwrap().contains(".syfrah/config.toml"));
    }

    // ── Serde roundtrip ──

    #[test]
    fn serde_roundtrip() {
        let c = Config::default();
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(
            back.daemon.health_check_interval,
            c.daemon.health_check_interval
        );
    }
}
