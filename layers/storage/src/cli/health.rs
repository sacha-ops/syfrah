//! Health and status subcommand handlers for `syfrah storage`.
//!
//! SECURITY: Never display S3 credentials (access key, secret key) in output.
//! Only the endpoint URL and bucket name are shown.

use std::path::PathBuf;

#[cfg(test)]
use crate::api::VolumeCacheStat;
use crate::api::{
    send_storage_request, StorageHealthReport, StorageRequest, StorageResponse, StorageStatusReport,
};

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

/// Run the `syfrah storage health` command.
pub async fn run_health(json: bool) -> anyhow::Result<()> {
    let req = StorageRequest::Health;
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Health(report) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            print_health_report(&report);
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Run the `syfrah storage status` command.
pub async fn run_status(json: bool) -> anyhow::Result<()> {
    let req = StorageRequest::Status;
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Status(report) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            print_status_report(&report);
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Pretty-print helpers
// ---------------------------------------------------------------------------

fn print_health_report(r: &StorageHealthReport) {
    let is_tty = console::Term::stdout().is_term();

    // -- S3 section --
    super::fmt::print_heading("S3 Backend", is_tty);
    super::fmt::print_kv("Endpoint", &r.s3_endpoint, is_tty);
    super::fmt::print_kv("Bucket", &r.s3_bucket, is_tty);
    super::fmt::print_kv(
        "Reachable",
        if r.s3_reachable { "yes" } else { "no" },
        is_tty,
    );
    super::fmt::print_kv(
        "Bucket Accessible",
        if r.bucket_accessible { "yes" } else { "no" },
        is_tty,
    );

    if let Some(ms) = r.put_latency_ms {
        super::fmt::print_kv("PUT Latency", &format!("{ms} ms"), is_tty);
    }
    if let Some(ms) = r.get_latency_ms {
        super::fmt::print_kv("GET Latency", &format!("{ms} ms"), is_tty);
    }
    if let Some(ms) = r.delete_latency_ms {
        super::fmt::print_kv("DELETE Latency", &format!("{ms} ms"), is_tty);
    }
    if let Some(ref err) = r.s3_error {
        super::fmt::print_kv("Error", err, is_tty);
    }

    println!();

    // -- Cache section --
    super::fmt::print_heading("Cache", is_tty);
    super::fmt::print_kv("Disk Path", &r.cache_disk_path, is_tty);
    super::fmt::print_kv(
        "Disk Total",
        &format_bytes(r.cache_disk_total_bytes),
        is_tty,
    );
    super::fmt::print_kv(
        "Disk Available",
        &format_bytes(r.cache_disk_available_bytes),
        is_tty,
    );
    super::fmt::print_kv(
        "Memory Limit",
        &format_bytes(r.cache_memory_limit_bytes),
        is_tty,
    );
}

fn print_status_report(r: &StorageStatusReport) {
    let is_tty = console::Term::stdout().is_term();

    super::fmt::print_heading("Storage Status", is_tty);
    super::fmt::print_kv("S3 Endpoint", &r.s3_endpoint, is_tty);
    super::fmt::print_kv(
        "S3 Connected",
        if r.s3_connected { "yes" } else { "no" },
        is_tty,
    );
    super::fmt::print_kv(
        "Total Dirty Bytes",
        &format_bytes(r.total_dirty_bytes),
        is_tty,
    );

    println!();

    if r.volume_cache_stats.is_empty() {
        println!("  (no volumes with cache data)");
    } else {
        super::fmt::print_heading("Per-Volume Cache", is_tty);
        let tw = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(120);

        let header = format!("{:<30} {:>14} {:>14}", "VOLUME", "CACHED", "DIRTY");
        if is_tty {
            let truncated = &header[..header.len().min(tw)];
            println!("{}", console::Style::new().bold().apply_to(truncated));
        } else {
            println!("{}", &header[..header.len().min(tw)]);
        }
        println!("{}", "-".repeat(60.min(tw)));

        for stat in &r.volume_cache_stats {
            let row = format!(
                "{:<30} {:>14} {:>14}",
                truncate(&stat.name, 30),
                format_bytes(stat.cached_bytes),
                format_bytes(stat.dirty_bytes),
            );
            println!("{}", &row[..row.len().min(tw)]);
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Format bytes into a human-readable string (e.g. "1.23 GiB").
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Truncate a string to `max` characters, appending "..." if it exceeds the limit.
///
/// Uses `char_indices` to avoid panicking on multi-byte UTF-8 strings.
fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{truncated}...")
    }
}

/// Build a user-friendly error when the daemon is unreachable.
fn daemon_connect_error(e: Box<dyn std::error::Error>) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon -- is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_kib() {
        assert_eq!(format_bytes(2048), "2.00 KiB");
    }

    #[test]
    fn format_bytes_gib() {
        assert_eq!(format_bytes(1_073_741_824), "1.00 GiB");
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_long() {
        assert_eq!(truncate("abcdefghij", 7), "abcd...");
    }

    #[test]
    fn truncate_multibyte_utf8() {
        // Multi-byte chars must not panic
        assert_eq!(truncate("aaaaaaaaaa", 7), "aaaa...");
        // Japanese chars (3 bytes each) — should not panic at byte boundary
        let jp = "\u{3042}\u{3044}\u{3046}\u{3048}\u{304A}"; // 5 chars
        assert_eq!(truncate(jp, 4), "\u{3042}...");
    }

    #[test]
    fn health_report_serialization() {
        let report = StorageHealthReport {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "test-bucket".into(),
            s3_reachable: true,
            bucket_accessible: true,
            put_latency_ms: Some(42),
            get_latency_ms: Some(15),
            delete_latency_ms: Some(10),
            s3_error: None,
            cache_disk_path: "/var/lib/syfrah/cache".into(),
            cache_disk_total_bytes: 107_374_182_400,
            cache_disk_available_bytes: 53_687_091_200,
            cache_memory_limit_bytes: 4_294_967_296,
        };
        let json = serde_json::to_string(&report).unwrap();
        // SECURITY: ensure no credential fields leak into output
        assert!(!json.contains("access_key"));
        assert!(!json.contains("secret_key"));
        assert!(json.contains("s3_endpoint"));
        assert!(json.contains("s3_bucket"));
    }

    #[test]
    fn status_report_serialization() {
        let report = StorageStatusReport {
            s3_connected: true,
            s3_endpoint: "https://s3.example.com".into(),
            volume_cache_stats: vec![VolumeCacheStat {
                name: "pgdata".into(),
                cached_bytes: 1_073_741_824,
                dirty_bytes: 524_288,
            }],
            total_dirty_bytes: 524_288,
            s3_put_latency_ms: Some(12),
            s3_get_latency_ms: Some(8),
            s3_degradation_level: Some("Healthy".into()),
            s3_outage_duration_secs: Some(0),
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("access_key"));
        assert!(!json.contains("secret_key"));
        assert!(json.contains("pgdata"));
    }

    #[test]
    fn health_report_no_credentials_in_debug() {
        let report = StorageHealthReport {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "test-bucket".into(),
            s3_reachable: false,
            bucket_accessible: false,
            put_latency_ms: None,
            get_latency_ms: None,
            delete_latency_ms: None,
            s3_error: Some("test error".into()),
            cache_disk_path: "/cache".into(),
            cache_disk_total_bytes: 0,
            cache_disk_available_bytes: 0,
            cache_memory_limit_bytes: 0,
        };
        let debug = format!("{report:?}");
        assert!(!debug.contains("access_key"));
        assert!(!debug.contains("secret_key"));
    }
}
