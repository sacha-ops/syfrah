//! `syfrah vpc create|list|delete|attach|detach|peer|unpeer|peerings` handlers.
//!
//! All operations go through the daemon's control socket.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::api::{send_org_request, OrgRequest, OrgResponse};
use crate::types::VpcOwner;

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
    org: &str,
    project: Option<&str>,
    shared: bool,
    cidr: Option<&str>,
) -> Result<()> {
    if !shared && project.is_none() {
        bail!(
            "--project is required for non-shared VPCs.\n\n\
             Usage:\n  \
             syfrah vpc create <name> --project <project> --org <org>\n  \
             syfrah vpc create <name> --org <org> --shared"
        );
    }

    let req = OrgRequest::VpcCreate {
        name: name.to_string(),
        org: org.to_string(),
        project: project.map(String::from),
        shared,
        cidr: cidr.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Vpc(vpc) => {
            println!("VPC created: {}", vpc.name);
            match &vpc.owner {
                VpcOwner::Org(org_id) => {
                    println!("  Org:      {}", org_id.0);
                    println!("  Shared:   yes");
                }
                VpcOwner::Project(proj_id) => {
                    println!("  Org:      {org}");
                    if let Some(p) = proj_id.0.split('/').nth(1) {
                        println!("  Project:  {p}");
                    }
                }
            }
            println!("  CIDR:     {}", vpc.cidr);
            println!("  VNI:      {}", vpc.vni);
            println!("  Created:  {}", format_timestamp(vpc.created_at));
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_list(org: Option<&str>, project: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::VpcList {
        org: org.map(String::from),
        project: project.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::VpcList(vpcs) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&vpcs)?);
                return Ok(());
            }

            if vpcs.is_empty() {
                println!("No VPCs found.");
                if let Some(org_name) = org {
                    println!(
                        "\nCreate one with: syfrah vpc create <name> --project <project> --org {org_name}"
                    );
                }
                return Ok(());
            }

            println!(
                "{:<20} {:<18} {:<6} {:<25} {:<8} CREATED",
                "NAME", "CIDR", "VNI", "OWNER", "SHARED"
            );
            println!("{}", "-".repeat(95));

            for vpc in &vpcs {
                let owner = match &vpc.owner {
                    VpcOwner::Project(pid) => pid.0.clone(),
                    VpcOwner::Org(oid) => oid.0.clone(),
                };
                let shared = if vpc.shared { "yes" } else { "no" };
                println!(
                    "{:<20} {:<18} {:<6} {:<25} {:<8} {}",
                    vpc.name,
                    vpc.cidr,
                    vpc.vni,
                    owner,
                    shared,
                    format_timestamp(vpc.created_at),
                );
            }

            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_delete(name: &str, _org: &str, yes: bool) -> Result<()> {
    if !yes {
        eprint!("Delete VPC '{name}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = OrgRequest::VpcDelete {
        name: name.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("VPC '{name}' deleted.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_attach(vpc_name: &str, project: &str) -> Result<()> {
    let req = OrgRequest::VpcAttach {
        vpc: vpc_name.to_string(),
        project: project.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Project '{project}' attached to VPC '{vpc_name}'.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_detach(vpc_name: &str, project: &str) -> Result<()> {
    let req = OrgRequest::VpcDetach {
        vpc: vpc_name.to_string(),
        project: project.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Project '{project}' detached from VPC '{vpc_name}'.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_peer(from: &str, to: &str) -> Result<()> {
    let req = OrgRequest::VpcPeer {
        from: from.to_string(),
        to: to.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("VPCs peered: {from} <-> {to}");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_unpeer(from: &str, to: &str) -> Result<()> {
    let req = OrgRequest::VpcUnpeer {
        from: from.to_string(),
        to: to.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("VPCs unpeered: {from} <-> {to}");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_peerings(vpc: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::VpcPeeringsList {
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::PeeringList(peerings) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&peerings)?);
                return Ok(());
            }

            if peerings.is_empty() {
                println!("No peerings found.");
                if vpc.is_some() {
                    println!("\nCreate one with: syfrah vpc peer --from <vpc-a> --to <vpc-b>");
                }
                return Ok(());
            }

            println!("{:<20} {:<20} {:<10} CREATED", "VPC_A", "VPC_B", "STATUS");
            println!("{}", "-".repeat(70));

            for p in &peerings {
                println!(
                    "{:<20} {:<20} {:<10} {}",
                    p.vpc_a,
                    p.vpc_b,
                    p.status,
                    format_timestamp(p.created_at),
                );
            }

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

    use super::super::VpcCommand;

    /// Helper to parse VPC commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: VpcCommand,
    }

    fn parse(args: &[&str]) -> VpcCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn vpc_create_parse() {
        let cmd = parse(&[
            "create",
            "my-vpc",
            "--project",
            "backend",
            "--org",
            "acme",
            "--cidr",
            "10.2.0.0/16",
        ]);
        match cmd {
            VpcCommand::Create {
                name,
                org,
                project,
                shared,
                cidr,
            } => {
                assert_eq!(name, "my-vpc");
                assert_eq!(org, "acme");
                assert_eq!(project.as_deref(), Some("backend"));
                assert!(!shared);
                assert_eq!(cidr.as_deref(), Some("10.2.0.0/16"));
            }
            other => panic!("expected Create, got {other:?}"),
        }

        let cmd = parse(&[
            "create",
            "shared-net",
            "--org",
            "acme",
            "--shared",
            "--cidr",
            "10.100.0.0/16",
        ]);
        match cmd {
            VpcCommand::Create {
                name,
                org,
                project,
                shared,
                cidr,
            } => {
                assert_eq!(name, "shared-net");
                assert_eq!(org, "acme");
                assert!(project.is_none());
                assert!(shared);
                assert_eq!(cidr.as_deref(), Some("10.100.0.0/16"));
            }
            other => panic!("expected Create, got {other:?}"),
        }

        let cmd = parse(&[
            "create",
            "default-vpc",
            "--project",
            "backend",
            "--org",
            "acme",
        ]);
        match cmd {
            VpcCommand::Create {
                name,
                project,
                cidr,
                shared,
                ..
            } => {
                assert_eq!(name, "default-vpc");
                assert_eq!(project.as_deref(), Some("backend"));
                assert!(cidr.is_none());
                assert!(!shared);
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn vpc_list_parse() {
        let cmd = parse(&["list", "--project", "backend", "--org", "acme", "--json"]);
        match cmd {
            VpcCommand::List { project, org, json } => {
                assert_eq!(project.as_deref(), Some("backend"));
                assert_eq!(org.as_deref(), Some("acme"));
                assert!(json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        let cmd = parse(&["list"]);
        match cmd {
            VpcCommand::List { project, org, json } => {
                assert!(project.is_none());
                assert!(org.is_none());
                assert!(!json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        let cmd = parse(&["list", "--org", "acme"]);
        match cmd {
            VpcCommand::List { project, org, json } => {
                assert!(project.is_none());
                assert_eq!(org.as_deref(), Some("acme"));
                assert!(!json);
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn vpc_peer_parse() {
        let cmd = parse(&["peer", "--from", "hub-vpc", "--to", "spoke-a"]);
        match cmd {
            VpcCommand::Peer { from, to } => {
                assert_eq!(from, "hub-vpc");
                assert_eq!(to, "spoke-a");
            }
            other => panic!("expected Peer, got {other:?}"),
        }
    }

    #[test]
    fn vpc_unpeer_parse() {
        let cmd = parse(&["unpeer", "--from", "hub-vpc", "--to", "spoke-a"]);
        match cmd {
            VpcCommand::Unpeer { from, to } => {
                assert_eq!(from, "hub-vpc");
                assert_eq!(to, "spoke-a");
            }
            other => panic!("expected Unpeer, got {other:?}"),
        }
    }

    #[test]
    fn vpc_peerings_parse() {
        let cmd = parse(&["peerings", "--vpc", "hub-vpc", "--json"]);
        match cmd {
            VpcCommand::Peerings { vpc, json } => {
                assert_eq!(vpc.as_deref(), Some("hub-vpc"));
                assert!(json);
            }
            other => panic!("expected Peerings, got {other:?}"),
        }

        let cmd = parse(&["peerings"]);
        match cmd {
            VpcCommand::Peerings { vpc, json } => {
                assert!(vpc.is_none());
                assert!(!json);
            }
            other => panic!("expected Peerings, got {other:?}"),
        }

        let cmd = parse(&["peerings", "--vpc", "my-vpc"]);
        match cmd {
            VpcCommand::Peerings { vpc, json } => {
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(!json);
            }
            other => panic!("expected Peerings, got {other:?}"),
        }
    }

    #[test]
    fn vpc_delete_parse() {
        let cmd = parse(&["delete", "my-vpc", "--org", "acme", "--yes"]);
        match cmd {
            VpcCommand::Delete { name, org, yes } => {
                assert_eq!(name, "my-vpc");
                assert_eq!(org, "acme");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "my-vpc", "--org", "acme"]);
        match cmd {
            VpcCommand::Delete { name, org, yes } => {
                assert_eq!(name, "my-vpc");
                assert_eq!(org, "acme");
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        let cmd = parse(&["delete", "-y", "my-vpc", "--org", "acme"]);
        match cmd {
            VpcCommand::Delete { name, org, yes } => {
                assert_eq!(name, "my-vpc");
                assert_eq!(org, "acme");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }
}
