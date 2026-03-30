//! CLI commands for `syfrah compute ...`.
//!
//! Provides subcommands for VM lifecycle management and compute layer
//! status queries. Each handler communicates with the daemon via the
//! control socket.

pub mod image;
pub mod vm;

use std::path::PathBuf;

use clap::Subcommand;

use crate::control::{send_compute_request, ComputeRequest, ComputeResponse};

/// Print a JSON error object to stdout and exit with code 1.
///
/// Used when `--json` is active so that callers parsing JSON output always
/// receive structured data, even on failure.
pub(crate) fn json_error_exit(msg: &str) -> ! {
    println!("{}", serde_json::json!({"error": msg}));
    std::process::exit(1)
}

/// Top-level compute CLI command.
#[derive(Debug, Subcommand)]
pub enum ComputeCommand {
    /// Manage virtual machines
    Vm {
        #[command(subcommand)]
        command: vm::VmCommand,
    },
    /// Manage images
    Image {
        #[command(subcommand)]
        command: image::ImageCommand,
    },
    /// Show compute layer status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Execute a compute CLI command.
pub async fn run(cmd: ComputeCommand) -> anyhow::Result<()> {
    match cmd {
        ComputeCommand::Vm { command } => vm::run(command).await,
        ComputeCommand::Image { command } => image::run(command).await,
        ComputeCommand::Status { json } => run_status(json).await,
    }
}

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

async fn run_status(json: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::Status;
    let resp = match send_compute_request(&control_socket_path(), &req).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>");
            if json {
                json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
    };

    match resp {
        ComputeResponse::Status(v) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("?");
                let runtime = v.get("runtime").and_then(|r| r.as_str()).unwrap_or("?");
                let total = v.get("total_vms").and_then(|t| t.as_u64()).unwrap_or(0);
                let running = v.get("running_vms").and_then(|r| r.as_u64()).unwrap_or(0);

                let is_tty = console::Term::stdout().is_term();
                if is_tty {
                    let bold = console::Style::new().bold().underlined();
                    println!("{}", bold.apply_to("Compute Status"));
                } else {
                    println!("Compute Status");
                    println!("{}", "=".repeat(14));
                }

                let print_kv = |key: &str, val: &str| {
                    if is_tty {
                        let bold = console::Style::new().bold();
                        println!("  {}: {val}", bold.apply_to(key));
                    } else {
                        println!("  {key}: {val}");
                    }
                };

                print_kv("Status", status);
                print_kv("Runtime", runtime);
                print_kv("Total VMs", &total.to_string());
                print_kv("Running VMs", &running.to_string());

                if let Some(warnings) = v.get("warnings").and_then(|w| w.as_array()) {
                    if !warnings.is_empty() {
                        if is_tty {
                            let yellow = console::Style::new().yellow();
                            println!("  {}:", yellow.apply_to("Warnings"));
                        } else {
                            println!("  Warnings:");
                        }
                        for w in warnings {
                            if let Some(msg) = w.as_str() {
                                println!("    - {msg}");
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            if json {
                json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
        _ => {
            if json {
                json_error_exit("unexpected response from daemon");
            }
            anyhow::bail!("unexpected response from daemon");
        }
    }
}
