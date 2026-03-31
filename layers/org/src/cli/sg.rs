//! `syfrah sg create|list|show|delete` handlers.
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

pub async fn run_create(name: &str, vpc: &str, description: &str) -> Result<()> {
    let req = OrgRequest::SgCreate {
        name: name.to_string(),
        vpc: vpc.to_string(),
        description: description.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Sg(sg) => {
            println!("Security group created: {}", sg.name);
            println!("  VPC:          {}", sg.vpc_id);
            println!("  Description:  {}", sg.description);
            println!("  State:        {}", sg.state);
            println!("  Created:      {}", format_timestamp(sg.created_at));
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_list(vpc: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::SgList {
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::SgList(sgs) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&sgs)?);
                return Ok(());
            }

            if sgs.is_empty() {
                println!("No security groups found.");
                if let Some(vpc_name) = vpc {
                    println!("\nCreate one with: syfrah sg create <name> --vpc {vpc_name}");
                }
                return Ok(());
            }

            println!(
                "{:<20} {:<20} {:<8} {:<10} {:<6} {:<4} CREATED",
                "NAME", "VPC", "DEFAULT", "STATE", "RULES", "VMs"
            );
            println!("{}", "-".repeat(88));

            for sg in &sgs {
                let default_str = if sg.is_default { "yes" } else { "no" };
                println!(
                    "{:<20} {:<20} {:<8} {:<10} {:<6} {:<4} {}",
                    sg.name,
                    sg.vpc_id,
                    default_str,
                    sg.state,
                    sg.rules.len(),
                    sg.attached_vms.len(),
                    format_timestamp(sg.created_at),
                );
            }

            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_show(name: &str, vpc: Option<&str>) -> Result<()> {
    let req = OrgRequest::SgShow {
        name: name.to_string(),
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Sg(sg) => {
            println!("Security Group: {}", sg.name);
            println!("  ID:           {}", sg.id);
            println!("  VPC:          {}", sg.vpc_id);
            println!("  Description:  {}", sg.description);
            let default_str = if sg.is_default { "yes" } else { "no" };
            println!("  Default:      {default_str}");
            println!("  State:        {}", sg.state);
            println!("  Created:      {}", format_timestamp(sg.created_at));
            println!("  Updated:      {}", format_timestamp(sg.updated_at));

            // Rules table
            println!();
            if sg.rules.is_empty() {
                println!("Rules: (none)");
            } else {
                println!("Rules:");
                println!(
                    "  {:<10} {:<8} {:<12} {:<20} {:<8} DESCRIPTION",
                    "DIRECTION", "PROTO", "PORTS", "SOURCE/DEST", "PRIORITY"
                );
                println!("  {}", "-".repeat(78));
                for rule in &sg.rules {
                    let ports = match &rule.port_range {
                        Some(pr) => pr.to_string(),
                        None => "-".to_string(),
                    };
                    let src_dst = if rule.direction == crate::types::Direction::Ingress {
                        &rule.source
                    } else {
                        &rule.destination
                    };
                    println!(
                        "  {:<10} {:<8} {:<12} {:<20} {:<8} {}",
                        rule.direction,
                        rule.protocol,
                        ports,
                        src_dst,
                        rule.priority,
                        rule.description,
                    );
                }
            }

            // Attached VMs
            println!();
            if sg.attached_vms.is_empty() {
                println!("Attached VMs: (none)");
            } else {
                println!("Attached VMs:");
                for vm in &sg.attached_vms {
                    println!("  - {vm}");
                }
            }

            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_delete(name: &str, vpc: Option<&str>, yes: bool) -> Result<()> {
    if !yes {
        eprint!("Delete security group '{name}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = OrgRequest::SgDelete {
        name: name.to_string(),
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Security group '{name}' deleted.");
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
    let days = ts / 86400;
    let remaining = ts % 86400;
    let hours = remaining / 3600;
    let mins = (remaining % 3600) / 60;
    let (year, month, day) = epoch_days_to_date(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{mins:02}")
}

fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::SgCommand;

    /// Helper to parse SG commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: SgCommand,
    }

    fn parse(args: &[&str]) -> SgCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn sg_create_parse() {
        let cmd = parse(&["create", "web-sg", "--vpc", "my-vpc"]);
        match cmd {
            SgCommand::Create {
                name,
                vpc,
                description,
            } => {
                assert_eq!(name, "web-sg");
                assert_eq!(vpc, "my-vpc");
                assert!(description.is_none());
            }
            other => panic!("expected Create, got {other:?}"),
        }

        let cmd = parse(&[
            "create",
            "db-sg",
            "--vpc",
            "prod-vpc",
            "--description",
            "Database tier",
        ]);
        match cmd {
            SgCommand::Create {
                name,
                vpc,
                description,
            } => {
                assert_eq!(name, "db-sg");
                assert_eq!(vpc, "prod-vpc");
                assert_eq!(description.as_deref(), Some("Database tier"));
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn sg_list_parse() {
        let cmd = parse(&["list", "--vpc", "my-vpc"]);
        match cmd {
            SgCommand::List { vpc, json } => {
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(!json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        let cmd = parse(&["list", "--vpc", "my-vpc", "--json"]);
        match cmd {
            SgCommand::List { vpc, json } => {
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(json);
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn sg_show_parse() {
        let cmd = parse(&["show", "web-sg"]);
        match cmd {
            SgCommand::Show { name, vpc } => {
                assert_eq!(name, "web-sg");
                assert!(vpc.is_none());
            }
            other => panic!("expected Show, got {other:?}"),
        }

        let cmd = parse(&["show", "web-sg", "--vpc", "my-vpc"]);
        match cmd {
            SgCommand::Show { name, vpc } => {
                assert_eq!(name, "web-sg");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
            }
            other => panic!("expected Show, got {other:?}"),
        }
    }

    #[test]
    fn sg_delete_parse() {
        let cmd = parse(&["delete", "web-sg", "--yes"]);
        match cmd {
            SgCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "web-sg");
                assert!(vpc.is_none());
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "web-sg"]);
        match cmd {
            SgCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "web-sg");
                assert!(vpc.is_none());
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "web-sg", "--vpc", "my-vpc", "-y"]);
        match cmd {
            SgCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "web-sg");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }
}
