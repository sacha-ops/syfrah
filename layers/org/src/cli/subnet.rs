//! `syfrah subnet create|list|delete` handlers.
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

pub async fn run_create(
    name: &str,
    env: &str,
    project: &str,
    org: &str,
    vpc: Option<&str>,
    cidr: Option<&str>,
) -> Result<()> {
    let req = OrgRequest::SubnetCreate {
        name: name.to_string(),
        env: env.to_string(),
        project: project.to_string(),
        org: org.to_string(),
        vpc: vpc.map(String::from),
        cidr: cidr.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Subnet(subnet) => {
            println!("Subnet created: {}", subnet.name);
            println!("  VPC:      {}", subnet.vpc_id);
            println!("  Env:      {env}");
            println!("  CIDR:     {}", subnet.cidr);
            println!("  Gateway:  {}", subnet.gateway);
            println!("  Created:  {}", format_timestamp(subnet.created_at));
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_list(
    env: Option<&str>,
    vpc: Option<&str>,
    project: Option<&str>,
    org: Option<&str>,
    json: bool,
) -> Result<()> {
    let req = OrgRequest::SubnetList {
        env: env.map(String::from),
        vpc: vpc.map(String::from),
        project: project.map(String::from),
        org: org.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::SubnetList(subnets) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&subnets)?);
                return Ok(());
            }

            if subnets.is_empty() {
                println!("No subnets found.");
                println!("\nCreate one with: syfrah subnet create <name> --env <env> --project <project> --org <org>");
                return Ok(());
            }

            println!(
                "{:<20} {:<25} {:<15} {:<18} {:<16} CREATED",
                "NAME", "VPC", "ENV", "CIDR", "GATEWAY"
            );
            println!("{}", "-".repeat(110));

            for subnet in &subnets {
                let env_name = subnet
                    .env_id
                    .0
                    .split('/')
                    .nth(2)
                    .unwrap_or(&subnet.env_id.0);

                println!(
                    "{:<20} {:<25} {:<15} {:<18} {:<16} {}",
                    subnet.name,
                    subnet.vpc_id,
                    env_name,
                    subnet.cidr,
                    subnet.gateway,
                    format_timestamp(subnet.created_at),
                );
            }

            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_delete(name: &str, vpc: Option<&str>, yes: bool) -> Result<()> {
    if !yes {
        let vpc_display = vpc.unwrap_or("(auto-detect)");
        eprint!("Delete subnet '{name}' from VPC '{vpc_display}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = OrgRequest::SubnetDelete {
        name: name.to_string(),
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Subnet '{name}' deleted.");
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

    use super::super::SubnetCommand;

    /// Helper to parse Subnet commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: SubnetCommand,
    }

    fn parse(args: &[&str]) -> SubnetCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn subnet_create_parse() {
        let cmd = parse(&[
            "create",
            "frontend",
            "--env",
            "production",
            "--project",
            "backend",
            "--org",
            "acme",
            "--vpc",
            "my-vpc",
            "--cidr",
            "10.1.1.0/24",
        ]);
        match cmd {
            SubnetCommand::Create {
                name,
                env,
                project,
                org,
                vpc,
                cidr,
            } => {
                assert_eq!(name, "frontend");
                assert_eq!(env, "production");
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert_eq!(cidr.as_deref(), Some("10.1.1.0/24"));
            }
            other => panic!("expected Create, got {other:?}"),
        }

        let cmd = parse(&[
            "create",
            "database",
            "--env",
            "production",
            "--project",
            "backend",
            "--org",
            "acme",
        ]);
        match cmd {
            SubnetCommand::Create {
                name,
                env,
                project,
                org,
                vpc,
                cidr,
            } => {
                assert_eq!(name, "database");
                assert_eq!(env, "production");
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                assert!(vpc.is_none());
                assert!(cidr.is_none());
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn subnet_list_parse() {
        let cmd = parse(&[
            "list",
            "--env",
            "production",
            "--vpc",
            "my-vpc",
            "--project",
            "backend",
            "--org",
            "acme",
            "--json",
        ]);
        match cmd {
            SubnetCommand::List {
                env,
                vpc,
                project,
                org,
                json,
            } => {
                assert_eq!(env.as_deref(), Some("production"));
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert_eq!(project.as_deref(), Some("backend"));
                assert_eq!(org.as_deref(), Some("acme"));
                assert!(json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        let cmd = parse(&["list"]);
        match cmd {
            SubnetCommand::List {
                env,
                vpc,
                project,
                org,
                json,
            } => {
                assert!(env.is_none());
                assert!(vpc.is_none());
                assert!(project.is_none());
                assert!(org.is_none());
                assert!(!json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        let cmd = parse(&["list", "--env", "staging"]);
        match cmd {
            SubnetCommand::List { env, vpc, .. } => {
                assert_eq!(env.as_deref(), Some("staging"));
                assert!(vpc.is_none());
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn subnet_delete_parse() {
        let cmd = parse(&["delete", "frontend", "--vpc", "my-vpc", "--yes"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "frontend", "--vpc", "my-vpc"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "-y", "frontend", "--vpc", "my-vpc"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "frontend", "--yes"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert!(vpc.is_none());
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }
}
