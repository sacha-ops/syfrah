//! `syfrah route` handlers.
//!
//! All operations go through the daemon's control socket.

use std::path::PathBuf;

use anyhow::Result;

use crate::api::{send_org_request, OrgRequest, OrgResponse};

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

fn daemon_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon — is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

// ── Route Table ──────────────────────────────────────────────────

pub async fn run_table_create(name: &str, vpc: &str) -> Result<()> {
    let req = OrgRequest::RouteTableCreate {
        name: name.to_string(),
        vpc: vpc.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::RouteTableResp(table) => {
            println!("Route table created: {}", table.name);
            println!("  VPC:      {}", table.vpc_id);
            println!("  Default:  {}", table.is_default);
            println!("  State:    {}", table.state);
            println!("  Created:  {}", format_timestamp(table.created_at));
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_table_list(vpc: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::RouteTableList {
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::RouteTableList(tables) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&tables)?);
                return Ok(());
            }

            if tables.is_empty() {
                println!("No route tables found.");
                return Ok(());
            }

            println!(
                "{:<20} {:<20} {:<10} {:<10} CREATED",
                "NAME", "VPC", "DEFAULT", "STATE"
            );
            println!("{}", "-".repeat(70));
            for t in &tables {
                println!(
                    "{:<20} {:<20} {:<10} {:<10} {}",
                    t.name,
                    t.vpc_id,
                    if t.is_default { "yes" } else { "no" },
                    t.state,
                    format_timestamp(t.created_at),
                );
            }
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_table_delete(name: &str, vpc: Option<&str>, yes: bool) -> Result<()> {
    if !yes {
        eprint!("Delete route table '{name}'? [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let req = OrgRequest::RouteTableDelete {
        name: name.to_string(),
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Route table '{name}' deleted.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_table_associate(table: &str, subnet: &str) -> Result<()> {
    let req = OrgRequest::RouteTableAssociate {
        table: table.to_string(),
        subnet: subnet.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Subnet '{subnet}' associated with route table '{table}'.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_table_disassociate(subnet: &str) -> Result<()> {
    let req = OrgRequest::RouteTableDisassociate {
        subnet: subnet.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!(
                "Subnet '{subnet}' disassociated from custom route table (using VPC default)."
            );
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

// ── Route ────────────────────────────────────────────────────────

pub async fn run_list(vpc: Option<&str>, table: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::RouteList {
        vpc: vpc.map(String::from),
        table: table.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::RouteListResp(routes) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&routes)?);
                return Ok(());
            }

            if routes.is_empty() {
                println!("No routes found.");
                return Ok(());
            }

            println!(
                "{:<20} {:<20} {:<12} {:<12} PRIORITY",
                "DESTINATION", "TARGET", "ORIGIN", "STATUS"
            );
            println!("{}", "-".repeat(76));
            for r in &routes {
                println!(
                    "{:<20} {:<20} {:<12} {:<12} {}",
                    r.destination, r.target, r.origin, r.status, r.priority,
                );
            }
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_add(
    vpc: &str,
    destination: &str,
    target: &str,
    table: Option<&str>,
    priority: Option<u32>,
) -> Result<()> {
    let req = OrgRequest::RouteAdd {
        vpc: vpc.to_string(),
        table: table.map(String::from),
        destination: destination.to_string(),
        target: target.to_string(),
        priority,
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::RouteResp(route) => {
            println!("Route added:");
            println!("  Destination: {}", route.destination);
            println!("  Target:      {}", route.target);
            println!("  Origin:      {}", route.origin);
            println!("  Status:      {}", route.status);
            println!("  Priority:    {}", route.priority);
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_delete(vpc: &str, destination: &str, table: Option<&str>) -> Result<()> {
    let req = OrgRequest::RouteDelete {
        vpc: vpc.to_string(),
        table: table.map(String::from),
        destination: destination.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Route for '{destination}' deleted.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

fn format_timestamp(ts: u64) -> String {
    if ts == 0 {
        return "-".to_string();
    }
    let secs = ts;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(secs);
    if age < 60 {
        format!("{age}s ago")
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86400)
    }
}
