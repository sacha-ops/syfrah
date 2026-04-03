//! Volume subcommand handlers.
//!
//! All operations go through the daemon's control socket.

use std::path::PathBuf;

use crate::api::{send_storage_request, StorageRequest, StorageResponse};

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

/// Create a new volume.
pub async fn run_create(
    name: &str,
    size_gb: u64,
    project: &str,
    org: &str,
    env: Option<&str>,
) -> anyhow::Result<()> {
    if size_gb == 0 {
        anyhow::bail!(
            "volume size must be at least 1 GB.\n\n\
             Usage: syfrah volume create {name} --size <GB> --project {project} --org {org}"
        );
    }

    let req = StorageRequest::VolumeCreate {
        name: name.to_string(),
        size_gb,
        project: project.to_string(),
        org: org.to_string(),
        env: env.map(|s| s.to_string()),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Volume(v) => {
            let vol_name = v["name"].as_str().unwrap_or(name);
            let vol_size = v["size_gb"].as_u64().unwrap_or(size_gb);
            println!("Volume '{vol_name}' created ({vol_size} GB).");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// List volumes.
pub async fn run_list(
    project: Option<&str>,
    org: Option<&str>,
    env: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let req = StorageRequest::VolumeList {
        project: project.map(|s| s.to_string()),
        org: org.map(|s| s.to_string()),
        env: env.map(|s| s.to_string()),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::VolumeList(volumes) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&volumes)?);
                return Ok(());
            }

            if volumes.is_empty() {
                println!("(no volumes)");
                println!();
                println!("Create one with: syfrah volume create <name> --size <GB> --project <project> --org <org>");
                return Ok(());
            }

            let tw = term_width();
            let header = format!(
                "{:<20} {:>8} {:<12} {:<16} {:<12}",
                "NAME", "SIZE", "STATE", "ATTACHED TO", "CREATED"
            );
            if console::Term::stdout().is_term() {
                let truncated = &header[..header.len().min(tw)];
                println!("{}", console::Style::new().bold().apply_to(truncated));
            } else {
                println!("{}", &header[..header.len().min(tw)]);
            }
            println!("{}", "-".repeat(70.min(tw)));

            for vol in &volumes {
                let name = vol["name"].as_str().unwrap_or("?");
                let size = vol["size_gb"]
                    .as_u64()
                    .map(|s| format!("{s} GB"))
                    .unwrap_or_else(|| "?".into());
                let state = vol["state"].as_str().unwrap_or("?");
                let attached = vol["attached_to"].as_str().unwrap_or("-");
                let created = vol["created_at"]
                    .as_u64()
                    .map(format_timestamp)
                    .unwrap_or_else(|| "-".into());
                let row = format!(
                    "{:<20} {:>8} {:<12} {:<16} {:<12}",
                    truncate(name, 20),
                    size,
                    state,
                    truncate(attached, 16),
                    created,
                );
                println!("{}", &row[..row.len().min(tw)]);
            }

            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Get volume details.
pub async fn run_get(name: &str, project: Option<&str>, json: bool) -> anyhow::Result<()> {
    let req = StorageRequest::VolumeGet {
        name: name.to_string(),
        project: project.map(|s| s.to_string()),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Volume(v) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }

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

            print_heading(&format!("Volume: {}", v["name"].as_str().unwrap_or(name)));
            print_kv("Name", v["name"].as_str().unwrap_or("?"));
            print_kv(
                "Size",
                &v["size_gb"]
                    .as_u64()
                    .map(|s| format!("{s} GB"))
                    .unwrap_or_else(|| "?".into()),
            );
            print_kv("State", v["state"].as_str().unwrap_or("?"));
            print_kv("Attached To", v["attached_to"].as_str().unwrap_or("(none)"));
            if let Some(org) = v["org"].as_str() {
                print_kv("Organization", org);
            }
            if let Some(project) = v["project"].as_str() {
                print_kv("Project", project);
            }
            if let Some(env) = v["env"].as_str() {
                print_kv("Environment", env);
            }
            print_kv(
                "Deletion Protection",
                if v["deletion_protection"].as_bool().unwrap_or(false) {
                    "enabled"
                } else {
                    "disabled"
                },
            );
            if let Some(ts) = v["created_at"].as_u64() {
                print_kv("Created", &format_timestamp(ts));
            }

            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Delete a volume.
pub async fn run_delete(
    name: &str,
    project: Option<&str>,
    cascade: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if !yes {
        let extra = if cascade {
            " and all its snapshots"
        } else {
            ""
        };
        eprint!("Delete volume '{name}'{extra}? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = StorageRequest::VolumeDelete {
        name: name.to_string(),
        project: project.map(|s| s.to_string()),
        cascade,
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Ok => {
            println!("Volume '{name}' deleted.");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Resize a volume.
pub async fn run_resize(name: &str, size_gb: u64, project: Option<&str>) -> anyhow::Result<()> {
    if size_gb == 0 {
        anyhow::bail!(
            "volume size must be at least 1 GB.\n\n\
             Usage: syfrah volume resize {name} --size <GB>"
        );
    }

    let req = StorageRequest::VolumeResize {
        name: name.to_string(),
        size_gb,
        project: project.map(|s| s.to_string()),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Volume(v) => {
            let new_size = v["size_gb"].as_u64().unwrap_or(size_gb);
            println!("Volume '{name}' resized to {new_size} GB.");
            Ok(())
        }
        StorageResponse::Ok => {
            println!("Volume '{name}' resized to {size_gb} GB.");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Update volume settings.
pub async fn run_update(
    name: &str,
    project: Option<&str>,
    deletion_protection: bool,
    no_deletion_protection: bool,
) -> anyhow::Result<()> {
    if !deletion_protection && !no_deletion_protection {
        anyhow::bail!(
            "nothing to update. Specify --deletion-protection or --no-deletion-protection.\n\n\
             Usage: syfrah volume update {name} --deletion-protection\n\
             Usage: syfrah volume update {name} --no-deletion-protection"
        );
    }

    let dp = if deletion_protection {
        Some(true)
    } else if no_deletion_protection {
        Some(false)
    } else {
        None
    };

    let req = StorageRequest::VolumeUpdate {
        name: name.to_string(),
        project: project.map(|s| s.to_string()),
        deletion_protection: dp,
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Ok | StorageResponse::Volume(_) => {
            let state = if deletion_protection {
                "enabled"
            } else {
                "disabled"
            };
            println!("Volume '{name}' updated: deletion protection {state}.");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a user-friendly error when the daemon is unreachable.
fn daemon_connect_error(e: Box<dyn std::error::Error>) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon -- is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

/// Return the current terminal width, falling back to 120 columns.
fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(120)
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

/// Format a Unix timestamp as a human-readable date string.
fn format_timestamp(ts: u64) -> String {
    let secs = ts;
    let days = secs / 86400;
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::VolumeCommand;

    /// Helper to parse volume commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: VolumeCommand,
    }

    fn parse(args: &[&str]) -> VolumeCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn volume_create_parse() {
        let cmd = parse(&[
            "create",
            "pgdata",
            "--size",
            "50",
            "--project",
            "backend",
            "--org",
            "acme",
        ]);
        match cmd {
            VolumeCommand::Create {
                name,
                size,
                project,
                org,
                env,
            } => {
                assert_eq!(name, "pgdata");
                assert_eq!(size, 50);
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                assert!(env.is_none());
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn volume_create_with_env() {
        let cmd = parse(&[
            "create",
            "pgdata",
            "--size",
            "50",
            "--project",
            "backend",
            "--org",
            "acme",
            "--env",
            "staging",
        ]);
        match cmd {
            VolumeCommand::Create { env, .. } => {
                assert_eq!(env.as_deref(), Some("staging"));
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn volume_list_parse_empty() {
        let cmd = parse(&["list"]);
        assert!(matches!(
            cmd,
            VolumeCommand::List {
                project: None,
                org: None,
                env: None,
                json: false,
            }
        ));
    }

    #[test]
    fn volume_list_parse_filters() {
        let cmd = parse(&["list", "--project", "backend", "--org", "acme", "--json"]);
        match cmd {
            VolumeCommand::List {
                project, org, json, ..
            } => {
                assert_eq!(project.as_deref(), Some("backend"));
                assert_eq!(org.as_deref(), Some("acme"));
                assert!(json);
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn volume_get_parse() {
        let cmd = parse(&["get", "pgdata"]);
        match cmd {
            VolumeCommand::Get {
                name,
                project,
                json,
            } => {
                assert_eq!(name, "pgdata");
                assert!(project.is_none());
                assert!(!json);
            }
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn volume_get_with_project_and_json() {
        let cmd = parse(&["get", "pgdata", "--project", "backend", "--json"]);
        match cmd {
            VolumeCommand::Get {
                name,
                project,
                json,
            } => {
                assert_eq!(name, "pgdata");
                assert_eq!(project.as_deref(), Some("backend"));
                assert!(json);
            }
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn volume_delete_parse() {
        let cmd = parse(&["delete", "pgdata", "--yes"]);
        match cmd {
            VolumeCommand::Delete {
                name,
                project,
                cascade,
                yes,
            } => {
                assert_eq!(name, "pgdata");
                assert!(project.is_none());
                assert!(!cascade);
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn volume_delete_cascade() {
        let cmd = parse(&["delete", "pgdata", "--cascade", "--yes"]);
        match cmd {
            VolumeCommand::Delete { cascade, .. } => {
                assert!(cascade);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn volume_resize_parse() {
        let cmd = parse(&["resize", "pgdata", "--size", "100"]);
        match cmd {
            VolumeCommand::Resize {
                name,
                size,
                project,
            } => {
                assert_eq!(name, "pgdata");
                assert_eq!(size, 100);
                assert!(project.is_none());
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    #[test]
    fn volume_update_deletion_protection() {
        let cmd = parse(&["update", "pgdata", "--deletion-protection"]);
        match cmd {
            VolumeCommand::Update {
                name,
                deletion_protection,
                no_deletion_protection,
                ..
            } => {
                assert_eq!(name, "pgdata");
                assert!(deletion_protection);
                assert!(!no_deletion_protection);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn volume_update_no_deletion_protection() {
        let cmd = parse(&["update", "pgdata", "--no-deletion-protection"]);
        match cmd {
            VolumeCommand::Update {
                deletion_protection,
                no_deletion_protection,
                ..
            } => {
                assert!(!deletion_protection);
                assert!(no_deletion_protection);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn volume_delete_short_yes() {
        let cmd = parse(&["delete", "-y", "pgdata"]);
        match cmd {
            VolumeCommand::Delete { name, yes, .. } => {
                assert_eq!(name, "pgdata");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }
}
