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

    let print_heading = |title: &str| {
        if is_tty {
            println!(
                "{}",
                console::Style::new().bold().underlined().apply_to(title)
            );
        } else {
            println!("{title}");
            println!("{}", "=".repeat(title.len()));
        }
    };

    let print_kv = |key: &str, val: &str| {
        if is_tty {
            println!("  {}: {val}", console::Style::new().bold().apply_to(key));
        } else {
            println!("  {key}: {val}");
        }
    };

    // -- S3 section --
    print_heading("S3 Backend");
    print_kv("Endpoint", &r.s3_endpoint);
    print_kv("Bucket", &r.s3_bucket);
    print_kv("Reachable", if r.s3_reachable { "yes" } else { "no" });
    print_kv(
        "Bucket Accessible",
        if r.bucket_accessible { "yes" } else { "no" },
    );

    if let Some(ms) = r.put_latency_ms {
        print_kv("PUT Latency", &format!("{ms} ms"));
    }
    if let Some(ms) = r.get_latency_ms {
        print_kv("GET Latency", &format!("{ms} ms"));
    }
    if let Some(ms) = r.delete_latency_ms {
        print_kv("DELETE Latency", &format!("{ms} ms"));
    }
    if let Some(ref err) = r.s3_error {
        print_kv("Error", err);
    }

    println!();

    // -- Cache section --
    print_heading("Cache");
    print_kv("Disk Path", &r.cache_disk_path);
    print_kv("Disk Total", &format_bytes(r.cache_disk_total_bytes));
    print_kv(
        "Disk Available",
        &format_bytes(r.cache_disk_available_bytes),
    );
    print_kv("Memory Limit", &format_bytes(r.cache_memory_limit_bytes));
}

fn print_status_report(r: &StorageStatusReport) {
    let is_tty = console::Term::stdout().is_term();

    let print_heading = |title: &str| {
        if is_tty {
            println!(
                "{}",
                console::Style::new().bold().underlined().apply_to(title)
            );
        } else {
            println!("{title}");
            println!("{}", "=".repeat(title.len()));
        }
    };

    let print_kv = |key: &str, val: &str| {
        if is_tty {
            println!("  {}: {val}", console::Style::new().bold().apply_to(key));
        } else {
            println!("  {key}: {val}");
        }
    };

    print_heading("Storage Status");
    print_kv("S3 Endpoint", &r.s3_endpoint);
    print_kv("S3 Connected", if r.s3_connected { "yes" } else { "no" });
    print_kv("Total Dirty Bytes", &format_bytes(r.total_dirty_bytes));

    println!();

    if r.volume_cache_stats.is_empty() {
        println!("  (no volumes with cache data)");
    } else {
        print_heading("Per-Volume Cache");
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
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 3 {
        s[..max].to_string()
    } else {
        format!("{}...", &s[..max - 3])
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
