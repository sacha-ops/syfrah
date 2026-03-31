//! `syfrah nat-gw` handlers.
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

pub async fn run_create(name: &str, vpc: &str, subnet: &str) -> Result<()> {
    let req = OrgRequest::NatGwCreate {
        name: name.to_string(),
        vpc: vpc.to_string(),
        subnet: subnet.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::NatGwResp(gw) => {
            println!("NAT Gateway created: {}", gw.name);
            println!("  VPC:       {}", gw.vpc_id);
            println!("  Subnet:    {}", gw.subnet_id);
            println!("  Public IP: {}", gw.public_ip);
            println!("  State:     {}", gw.state);
            println!("  Created:   {}", format_timestamp(gw.created_at));
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_list(vpc: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::NatGwList {
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::NatGwList(gws) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&gws)?);
                return Ok(());
            }

            if gws.is_empty() {
                println!("No NAT gateways found.");
                return Ok(());
            }

            println!(
                "{:<20} {:<25} {:<18} {:<10} CREATED",
                "NAME", "VPC", "PUBLIC IP", "STATE"
            );
            println!("{}", "-".repeat(83));
            for gw in &gws {
                println!(
                    "{:<20} {:<25} {:<18} {:<10} {}",
                    gw.name,
                    gw.vpc_id,
                    gw.public_ip,
                    gw.state,
                    format_timestamp(gw.created_at),
                );
            }
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_show(name: &str) -> Result<()> {
    let req = OrgRequest::NatGwShow {
        name: name.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::NatGwResp(gw) => {
            println!("Name:       {}", gw.name);
            println!("ID:         {}", gw.id);
            println!("VPC:        {}", gw.vpc_id);
            println!("Subnet:     {}", gw.subnet_id);
            println!("Public IP:  {}", gw.public_ip);
            println!("State:      {}", gw.state);
            println!("Created:    {}", format_timestamp(gw.created_at));
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_delete(name: &str, yes: bool) -> Result<()> {
    if !yes {
        eprint!("Delete NAT gateway '{name}'? [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let req = OrgRequest::NatGwDelete {
        name: name.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("NAT gateway '{name}' deleted.");
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(ts);
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
