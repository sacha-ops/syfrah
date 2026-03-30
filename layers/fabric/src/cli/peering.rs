use crate::control::{send_control_request, FabricRequest, FabricResponse};
use crate::sanitize::sanitize;
use crate::ui;
use crate::{no_mesh_error, store};
use anyhow::Result;
use std::collections::HashSet;

/// Interactive peering mode: watch for requests and prompt accept/reject.
///
/// In default mode (`continuous=false`), exits after the first accept/reject.
/// With `--watch` (`continuous=true`), loops indefinitely for batch use.
pub async fn watch(pin: Option<String>, continuous: bool) -> Result<()> {
    // Load mesh state (fails fast with a friendly message if no mesh exists).
    let state = store::load().map_err(|_| no_mesh_error())?;
    let port = state.peering_port;

    // Start peering with optional PIN
    let resp = send_request(FabricRequest::PeeringStart {
        port,
        pin: pin.clone(),
    })
    .await?;
    match resp {
        FabricResponse::Ok => {}
        FabricResponse::Error { message } => anyhow::bail!("{message}"),
        _ => {}
    }

    ui::peering_banner(port, pin.as_deref(), continuous);

    // Poll for new requests; handle Ctrl+C gracefully
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nPeering watch stopped. Daemon still running.");
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }

        let resp = match send_request(FabricRequest::PeeringList).await {
            Ok(r) => r,
            Err(_) => continue,
        };

        if let FabricResponse::PeeringList { requests } = resp {
            for req in &requests {
                if seen.contains(&req.request_id) {
                    continue;
                }
                seen.insert(req.request_id.clone());

                let key_prefix = &req.wg_public_key[..20.min(req.wg_public_key.len())];
                ui::join_request_card(
                    &sanitize(&req.node_name),
                    &req.endpoint.to_string(),
                    key_prefix,
                );

                // Read from stdin
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut input = String::new();
                if std::io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    if trimmed.is_empty() || trimmed == "y" || trimmed == "yes" {
                        match send_request(FabricRequest::PeeringAccept {
                            request_id: req.request_id.clone(),
                        })
                        .await
                        {
                            Ok(FabricResponse::PeeringAccepted { peer_name }) => {
                                if ui::use_color() {
                                    let green = console::Style::new().green();
                                    println!(
                                        "     {} {} joined the mesh.\n",
                                        green.apply_to("\u{2713}"),
                                        sanitize(&peer_name)
                                    );
                                } else {
                                    println!(
                                        "  Accepted: {} joined the mesh.\n",
                                        sanitize(&peer_name)
                                    );
                                }
                            }
                            Ok(FabricResponse::Error { message }) => {
                                ui::warn(&format!("Error: {message}"));
                                println!();
                            }
                            _ => {}
                        }
                    } else {
                        match send_request(FabricRequest::PeeringReject {
                            request_id: req.request_id.clone(),
                            reason: Some("rejected by operator".into()),
                        })
                        .await
                        {
                            Ok(_) => println!("  Rejected.\n"),
                            Err(e) => {
                                ui::warn(&format!("Error: {e}"));
                                println!();
                            }
                        }
                    }

                    // In default (non-watch) mode, exit after handling the first request
                    if !continuous {
                        return Ok(());
                    }
                }
            }
        }
    }
}

pub async fn start(port: u16, pin: Option<String>) -> Result<()> {
    let sp = ui::spinner(&format!("Starting peering on port {port}..."));
    let resp = send_request(FabricRequest::PeeringStart {
        port,
        pin: pin.clone(),
    })
    .await?;
    match resp {
        FabricResponse::Ok => {
            if let Some(ref p) = pin {
                ui::step_ok(&sp, &format!("Peering started on port {port}"));
                println!("  Mode: auto-accept with PIN");
                println!("  Nodes can join with: syfrah fabric join <this-ip> --pin {p}");
            } else {
                ui::step_ok(&sp, &format!("Peering started on port {port}"));
                println!("  Mode: manual approval (you must accept each join request)");
            }
        }
        FabricResponse::Error { message } => {
            ui::step_fail(&sp, &format!("Failed: {message}"));
            anyhow::bail!("{message}");
        }
        _ => {
            ui::step_fail(&sp, "Unexpected response");
            anyhow::bail!("unexpected response");
        }
    }
    Ok(())
}

pub async fn stop() -> Result<()> {
    let sp = ui::spinner("Stopping peering...");
    let resp = send_request(FabricRequest::PeeringStop).await?;
    match resp {
        FabricResponse::Ok => ui::step_ok(&sp, "Peering stopped."),
        FabricResponse::Error { message } => {
            ui::step_fail(&sp, &format!("Failed: {message}"));
            anyhow::bail!("{message}");
        }
        _ => {
            ui::step_fail(&sp, "Unexpected response");
            anyhow::bail!("unexpected response");
        }
    }
    Ok(())
}

pub async fn list() -> Result<()> {
    let resp = send_request(FabricRequest::PeeringList).await?;
    match resp {
        FabricResponse::PeeringList { requests } => {
            if requests.is_empty() {
                println!("No pending join requests.");
            } else {
                println!(
                    "{:<10} {:<16} {:<22} {:<20}",
                    "ID", "NAME", "ENDPOINT", "WG PUBKEY"
                );
                println!("{}", "-".repeat(70));
                for r in &requests {
                    println!(
                        "{:<10} {:<16} {:<22} {:<20}",
                        r.request_id,
                        truncate(&sanitize(&r.node_name), 15),
                        r.endpoint,
                        truncate(&r.wg_public_key, 19),
                    );
                }
                println!("\n{} pending request(s)", requests.len());
            }
        }
        FabricResponse::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("unexpected response"),
    }
    Ok(())
}

pub async fn accept(request_id: &str) -> Result<()> {
    let sp = ui::spinner(&format!("Accepting request {request_id}..."));
    let resp = send_request(FabricRequest::PeeringAccept {
        request_id: request_id.to_string(),
    })
    .await?;
    match resp {
        FabricResponse::PeeringAccepted { peer_name } => {
            ui::step_ok(&sp, &format!("{} joined the mesh.", sanitize(&peer_name)));
        }
        FabricResponse::Error { message } => {
            ui::step_fail(&sp, &format!("Failed: {message}"));
            anyhow::bail!("{message}");
        }
        _ => {
            ui::step_fail(&sp, "Unexpected response");
            anyhow::bail!("unexpected response");
        }
    }
    Ok(())
}

pub async fn reject(request_id: &str, reason: Option<String>) -> Result<()> {
    let sp = ui::spinner(&format!("Rejecting request {request_id}..."));
    let resp = send_request(FabricRequest::PeeringReject {
        request_id: request_id.to_string(),
        reason,
    })
    .await?;
    match resp {
        FabricResponse::Ok => ui::step_ok(&sp, &format!("Request {request_id} rejected.")),
        FabricResponse::Error { message } => {
            ui::step_fail(&sp, &format!("Failed: {message}"));
            anyhow::bail!("{message}");
        }
        _ => {
            ui::step_fail(&sp, "Unexpected response");
            anyhow::bail!("unexpected response");
        }
    }
    Ok(())
}

async fn send_request(req: FabricRequest) -> Result<FabricResponse> {
    let path = store::control_socket_path();
    if !path.exists() {
        anyhow::bail!("daemon not running. Start with 'syfrah fabric start' first.");
    }
    let resp = send_control_request(&path, &req)
        .await
        .map_err(|e| anyhow::anyhow!("failed to communicate with daemon: {e}"))?;
    Ok(resp)
}

use super::ui::truncate;

#[cfg(test)]
mod tests {
    use super::*;

    /// `watch()` must fail immediately when no mesh is configured
    /// instead of silently auto-initialising one.
    #[tokio::test]
    async fn watch_errors_without_mesh() {
        // Point HOME at a fresh temp dir so store::exists() returns false.
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let err = watch(None, false).await.unwrap_err();
        assert!(
            err.to_string().contains(
                "No mesh configured. Run 'syfrah fabric init' or 'syfrah fabric join' first."
            ),
            "unexpected error: {err}"
        );
    }
}
