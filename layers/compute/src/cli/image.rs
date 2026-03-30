//! Image management CLI commands.
//!
//! Subcommands: list, inspect, pull, import, delete, catalog.
//! Each handler communicates with the daemon via the control socket.

use std::path::PathBuf;
use std::time::Instant;

use clap::Subcommand;
use indicatif::{ProgressBar, ProgressStyle};

use crate::control::{send_compute_request, ComputeRequest, ComputeResponse};

/// Image management subcommands.
#[derive(Debug, Subcommand)]
pub enum ImageCommand {
    /// List locally available images
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show detailed metadata for an image
    Inspect {
        /// Image name
        name: String,
        /// Output as JSON (default for inspect; accepted for consistency)
        #[arg(long)]
        json: bool,
    },
    /// Download an image from the catalog
    #[command(after_help = "Examples:\n  syfrah compute image pull alpine-3.20")]
    Pull {
        /// Image name
        name: String,
    },
    /// Import a local raw disk image
    #[command(
        after_help = "Examples:\n  syfrah compute image import /tmp/disk.raw --name custom-os"
    )]
    Import {
        /// Path to the raw disk image file
        path: PathBuf,
        /// Name to assign to the imported image
        #[arg(long)]
        name: String,
        /// CPU architecture (default: amd64)
        #[arg(long, default_value = "amd64")]
        arch: String,
    },
    /// Delete a locally cached image
    Delete {
        /// Image name
        name: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Show remote image catalog
    #[command(
        after_help = "Examples:\n  syfrah compute image catalog\n  syfrah compute image catalog --json"
    )]
    Catalog {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Execute an image subcommand.
pub async fn run(cmd: ImageCommand) -> anyhow::Result<()> {
    match cmd {
        ImageCommand::List { json } => run_list(json).await,
        ImageCommand::Inspect { name, json } => run_inspect(name, json).await,
        ImageCommand::Pull { name } => run_pull(name).await,
        ImageCommand::Import { path, name, arch } => run_import(path, name, arch).await,
        ImageCommand::Delete { name, yes } => run_delete(name, yes).await,
        ImageCommand::Catalog { json } => run_catalog(json).await,
    }
}

// ---------------------------------------------------------------------------
// Control socket path
// ---------------------------------------------------------------------------

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn run_list(json: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::ImageList;
    let resp = match send_compute_request(&control_socket_path(), &req).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>");
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
    };

    match resp {
        ComputeResponse::ImageList(images) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&images)?);
            } else {
                let header = format!(
                    "{:<25} {:<10} {:<10} {:<12} {:<10}",
                    "NAME", "ARCH", "SIZE MB", "CLOUD-INIT", "SOURCE"
                );
                if console::Term::stdout().is_term() {
                    println!("{}", console::Style::new().bold().apply_to(&header));
                } else {
                    println!("{header}");
                }
                println!("{}", "-".repeat(67));
                if images.is_empty() {
                    println!("(no images)");
                } else {
                    for img in &images {
                        let name = img.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        let arch = img.get("arch").and_then(|a| a.as_str()).unwrap_or("?");
                        let size = img.get("size_mb").and_then(|s| s.as_u64()).unwrap_or(0);
                        let ci = img
                            .get("cloud_init")
                            .and_then(|c| c.as_bool())
                            .map(|b| if b { "yes" } else { "no" })
                            .unwrap_or("?");
                        let source = img
                            .get("source_kind")
                            .and_then(|s| s.as_str())
                            .unwrap_or("?");
                        println!("{name:<25} {arch:<10} {size:<10} {ci:<12} {source:<10}");
                    }
                }
            }
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
        _ => {
            if json {
                super::json_error_exit("unexpected response from daemon");
            }
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_inspect(name: String, json: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::ImageInspect { name };
    let resp = match send_compute_request(&control_socket_path(), &req).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>");
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
    };

    match resp {
        ComputeResponse::ImageMeta(v) => {
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
        _ => {
            if json {
                super::json_error_exit("unexpected response from daemon");
            }
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_pull(name: String) -> anyhow::Result<()> {
    let sock = control_socket_path();

    // Fetch catalog to get image size before starting download.
    let size_mb = match send_compute_request(&sock, &ComputeRequest::ImageCatalog).await {
        Ok(ComputeResponse::ImageCatalog(v)) => v
            .get("images")
            .and_then(|i| i.as_array())
            .and_then(|imgs| {
                imgs.iter()
                    .find(|img| img.get("name").and_then(|n| n.as_str()) == Some(&name))
            })
            .and_then(|img| img.get("size_mb").and_then(|s| s.as_u64())),
        _ => None,
    };

    // Use a determinate progress bar when the image size is known from the
    // catalog, otherwise fall back to a spinner with a byte counter.
    let bar = if let Some(mb) = size_mb {
        let total_bytes = mb * 1024 * 1024;
        let pb = ProgressBar::new(total_bytes);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.cyan} {msg} [{wide_bar:.cyan/dim}] {bytes}/{total_bytes}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("##-"),
        );
        pb.set_message(format!("Downloading {name}"));
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.set_message(format!("Downloading {name}..."));
        pb
    };
    bar.enable_steady_tick(std::time::Duration::from_millis(120));

    // Simulate download progress when we know the total size. The actual
    // download happens inside the daemon behind the control socket so we
    // cannot get real byte-level feedback. Instead we advance the bar
    // smoothly up to ~90 % while waiting for the daemon response and then
    // jump to 100 % on completion.
    let bar_handle = bar.clone();
    let has_size = size_mb.is_some();
    let tick_task = if has_size {
        let total = size_mb.unwrap() * 1024 * 1024;
        Some(tokio::spawn(async move {
            // Advance to ~90 % over an estimated duration. We estimate
            // ~20 MB/s as a conservative baseline so the bar never stalls
            // for too long on fast downloads.
            let target = (total as f64 * 0.9) as u64;
            let estimated_secs = (total as f64 / (20.0 * 1024.0 * 1024.0)).max(1.0);
            let steps = 100u64;
            let step_bytes = target / steps;
            let step_dur = std::time::Duration::from_secs_f64(estimated_secs / steps as f64);

            for _ in 0..steps {
                tokio::time::sleep(step_dur).await;
                bar_handle.inc(step_bytes);
                if bar_handle.is_finished() {
                    return;
                }
            }
        }))
    } else {
        None
    };

    let start = Instant::now();

    let req = ComputeRequest::ImagePull { name: name.clone() };
    let resp = send_compute_request(&sock, &req).await.map_err(|e| {
        bar.finish_and_clear();
        anyhow::anyhow!(
            "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
        )
    })?;

    // Stop the tick task before finishing the bar.
    if let Some(task) = tick_task {
        task.abort();
    }

    let elapsed = start.elapsed().as_secs_f64();

    match resp {
        ComputeResponse::ImageMeta(_) => {
            if has_size {
                let total = size_mb.unwrap() * 1024 * 1024;
                bar.set_position(total);
            }
            bar.finish_and_clear();
            let size_label = match size_mb {
                Some(mb) => format!(" ({mb} MB)"),
                None => String::new(),
            };
            let time_str = if elapsed < 1.0 {
                format!("{:.0}ms", elapsed * 1000.0)
            } else {
                format!("{elapsed:.1}s")
            };
            println!("Downloaded {name}{size_label} in {time_str}. SHA256 verified.");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            bar.finish_and_clear();
            anyhow::bail!("{msg}");
        }
        _ => {
            bar.finish_and_clear();
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_import(path: PathBuf, name: String, arch: String) -> anyhow::Result<()> {
    let req = ComputeRequest::ImageImport {
        path: path.clone(),
        name: name.clone(),
        arch,
    };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::ImageMeta(v) => {
            let size = v.get("size_mb").and_then(|s| s.as_u64()).unwrap_or(0);
            let arch = v.get("arch").and_then(|a| a.as_str()).unwrap_or("?");
            println!("Imported {name} ({size} MB, {arch})");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_delete(name: String, yes: bool) -> anyhow::Result<()> {
    // Check that the image exists before prompting for confirmation.
    let inspect_req = ComputeRequest::ImageInspect { name: name.clone() };
    let inspect_resp = send_compute_request(&control_socket_path(), &inspect_req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match inspect_resp {
        ComputeResponse::ImageMeta(_) => {} // Image exists, proceed
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }

    if !yes {
        eprint!("Delete image {name}? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = ComputeRequest::ImageDelete { name: name.clone() };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Ok => {
            println!("Deleted {name}");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_catalog(json: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::ImageCatalog;
    let resp = match send_compute_request(&control_socket_path(), &req).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>");
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
    };

    match resp {
        ComputeResponse::ImageCatalog(v) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            let images = v.get("images").and_then(|i| i.as_array());
            match images {
                Some(images) => {
                    let header = format!(
                        "{:<25} {:<10} {:<10} {:<12}",
                        "NAME", "ARCH", "SIZE MB", "CLOUD-INIT"
                    );
                    if console::Term::stdout().is_term() {
                        println!("{}", console::Style::new().bold().apply_to(&header));
                    } else {
                        println!("{header}");
                    }
                    println!("{}", "-".repeat(57));
                    if images.is_empty() {
                        println!("(no images in catalog)");
                    } else {
                        for img in images {
                            let name = img.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let arch = img.get("arch").and_then(|a| a.as_str()).unwrap_or("?");
                            let size = img.get("size_mb").and_then(|s| s.as_u64()).unwrap_or(0);
                            let ci = img
                                .get("cloud_init")
                                .and_then(|c| c.as_bool())
                                .map(|b| if b { "yes" } else { "no" })
                                .unwrap_or("?");
                            println!("{name:<25} {arch:<10} {size:<10} {ci:<12}");
                        }
                    }
                }
                None => {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                }
            }
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
        _ => {
            if json {
                super::json_error_exit("unexpected response from daemon");
            }
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// Helper to parse image commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: ImageCommand,
    }

    fn parse(args: &[&str]) -> ImageCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn parse_list() {
        let cmd = parse(&["list"]);
        assert!(matches!(cmd, ImageCommand::List { json: false }));
    }

    #[test]
    fn parse_list_json() {
        let cmd = parse(&["list", "--json"]);
        assert!(matches!(cmd, ImageCommand::List { json: true }));
    }

    #[test]
    fn parse_inspect() {
        let cmd = parse(&["inspect", "ubuntu-24.04"]);
        match cmd {
            ImageCommand::Inspect { name, json } => {
                assert_eq!(name, "ubuntu-24.04");
                assert!(!json);
            }
            other => panic!("expected Inspect, got {other:?}"),
        }
    }

    #[test]
    fn parse_inspect_json() {
        let cmd = parse(&["inspect", "ubuntu-24.04", "--json"]);
        match cmd {
            ImageCommand::Inspect { name, json } => {
                assert_eq!(name, "ubuntu-24.04");
                assert!(json);
            }
            other => panic!("expected Inspect, got {other:?}"),
        }
    }

    #[test]
    fn parse_pull() {
        let cmd = parse(&["pull", "alpine-3.20"]);
        match cmd {
            ImageCommand::Pull { name } => assert_eq!(name, "alpine-3.20"),
            other => panic!("expected Pull, got {other:?}"),
        }
    }

    #[test]
    fn parse_import() {
        let cmd = parse(&["import", "/tmp/disk.raw", "--name", "custom-os"]);
        match cmd {
            ImageCommand::Import { path, name, arch } => {
                assert_eq!(path, PathBuf::from("/tmp/disk.raw"));
                assert_eq!(name, "custom-os");
                assert_eq!(arch, "amd64"); // default
            }
            other => panic!("expected Import, got {other:?}"),
        }
    }

    #[test]
    fn parse_import_with_arch() {
        let cmd = parse(&[
            "import",
            "/tmp/disk.raw",
            "--name",
            "custom-os",
            "--arch",
            "aarch64",
        ]);
        match cmd {
            ImageCommand::Import { path, name, arch } => {
                assert_eq!(path, PathBuf::from("/tmp/disk.raw"));
                assert_eq!(name, "custom-os");
                assert_eq!(arch, "aarch64");
            }
            other => panic!("expected Import, got {other:?}"),
        }
    }

    #[test]
    fn parse_delete() {
        let cmd = parse(&["delete", "old-image"]);
        match cmd {
            ImageCommand::Delete { name, yes } => {
                assert_eq!(name, "old-image");
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn parse_delete_yes() {
        let cmd = parse(&["delete", "--yes", "old-image"]);
        match cmd {
            ImageCommand::Delete { name, yes } => {
                assert_eq!(name, "old-image");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn parse_delete_short_y() {
        let cmd = parse(&["delete", "-y", "old-image"]);
        match cmd {
            ImageCommand::Delete { name, yes } => {
                assert_eq!(name, "old-image");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn parse_catalog() {
        let cmd = parse(&["catalog"]);
        assert!(matches!(cmd, ImageCommand::Catalog { json: false }));
    }

    #[test]
    fn parse_catalog_json() {
        let cmd = parse(&["catalog", "--json"]);
        assert!(matches!(cmd, ImageCommand::Catalog { json: true }));
    }
}
