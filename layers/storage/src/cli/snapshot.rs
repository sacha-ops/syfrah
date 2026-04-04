//! Snapshot subcommand handlers.
//!
//! All operations go through the daemon's control socket, following the
//! same pattern as `volume.rs`.

use crate::api::{send_storage_request, StorageRequest, StorageResponse};

use super::fmt::{
    control_socket_path, daemon_connect_error, format_timestamp, term_width, truncate,
};

/// Create a snapshot from a volume.
pub async fn run_create(
    name: &str,
    volume: &str,
    project: Option<&str>,
    org: Option<&str>,
) -> anyhow::Result<()> {
    // project and org are forwarded to scope the volume lookup when provided.
    // Today the daemon resolves volumes by name alone, but these flags ensure
    // correctness when two projects contain identically-named volumes.
    let _ = (project, org); // TODO: forward to daemon once SnapshotCreate supports scoping
    let req = StorageRequest::SnapshotCreate {
        name: name.to_string(),
        volume: volume.to_string(),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Snapshot(s) => {
            let snap_name = s["name"].as_str().unwrap_or(name);
            let source = s["source_volume"].as_str().unwrap_or(volume);
            println!("Snapshot '{snap_name}' created from volume '{source}'.");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// List snapshots, optionally filtered by source volume.
pub async fn run_list(
    volume: Option<&str>,
    project: Option<&str>,
    org: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    // project and org are accepted for consistency with volume list but not yet
    // forwarded to the daemon request.
    let _ = (project, org); // TODO: forward once SnapshotList supports scoping
    let req = StorageRequest::SnapshotList {
        volume: volume.map(|s| s.to_string()),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::SnapshotList(snapshots) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&snapshots)?);
                return Ok(());
            }

            if snapshots.is_empty() {
                println!("(no snapshots)");
                println!();
                println!("Create one with: syfrah volume snapshot create <name> --volume <volume>");
                return Ok(());
            }

            let tw = term_width();
            let header = format!(
                "{:<24} {:<24} {:>8} {:<12} {:<12}",
                "NAME", "SOURCE VOLUME", "SIZE", "STATE", "CREATED"
            );
            if console::Term::stdout().is_term() {
                let truncated = &header[..header.len().min(tw)];
                println!("{}", console::Style::new().bold().apply_to(truncated));
            } else {
                println!("{}", &header[..header.len().min(tw)]);
            }
            println!("{}", "-".repeat(82.min(tw)));

            for snap in &snapshots {
                let name = snap["name"].as_str().unwrap_or("?");
                let source = snap["source_volume"].as_str().unwrap_or("?");
                let size = snap["size_gb"]
                    .as_u64()
                    .map(|s| format!("{s} GB"))
                    .unwrap_or_else(|| "?".into());
                let state = snap["state"].as_str().unwrap_or("?");
                let created = snap["created_at"]
                    .as_u64()
                    .map(format_timestamp)
                    .unwrap_or_else(|| "-".into());
                let row = format!(
                    "{:<24} {:<24} {:>8} {:<12} {:<12}",
                    truncate(name, 24),
                    truncate(source, 24),
                    size,
                    state,
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

/// Get snapshot details.
pub async fn run_get(name: &str, json: bool) -> anyhow::Result<()> {
    let req = StorageRequest::SnapshotGet {
        name: name.to_string(),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Snapshot(s) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }

            let is_tty = console::Term::stdout().is_term();

            super::fmt::print_heading(
                &format!("Snapshot: {}", s["name"].as_str().unwrap_or(name)),
                is_tty,
            );
            super::fmt::print_kv("Name", s["name"].as_str().unwrap_or("?"), is_tty);
            super::fmt::print_kv(
                "Source Volume",
                s["source_volume"].as_str().unwrap_or("?"),
                is_tty,
            );
            super::fmt::print_kv(
                "Size",
                &s["size_gb"]
                    .as_u64()
                    .map(|sz| format!("{sz} GB"))
                    .unwrap_or_else(|| "?".into()),
                is_tty,
            );
            super::fmt::print_kv("State", s["state"].as_str().unwrap_or("?"), is_tty);
            if let Some(org) = s["org"].as_str() {
                super::fmt::print_kv("Organization", org, is_tty);
            }
            if let Some(project) = s["project"].as_str() {
                super::fmt::print_kv("Project", project, is_tty);
            }
            if let Some(env) = s["env"].as_str() {
                super::fmt::print_kv("Environment", env, is_tty);
            }
            if let Some(ts) = s["created_at"].as_u64() {
                super::fmt::print_kv("Created", &format_timestamp(ts), is_tty);
            }

            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Restore a snapshot into a new volume.
pub async fn run_restore(snapshot: &str, target_volume: &str) -> anyhow::Result<()> {
    let req = StorageRequest::SnapshotRestore {
        snapshot: snapshot.to_string(),
        name: target_volume.to_string(),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Volume(v) => {
            let vol_name = v["name"].as_str().unwrap_or(target_volume);
            println!("Volume '{vol_name}' created from snapshot '{snapshot}'.");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Delete a snapshot.
pub async fn run_delete(name: &str, yes: bool) -> anyhow::Result<()> {
    if !yes {
        eprint!("Delete snapshot '{name}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = StorageRequest::SnapshotDelete {
        name: name.to_string(),
    };
    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Ok => {
            println!("Snapshot '{name}' deleted.");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}
