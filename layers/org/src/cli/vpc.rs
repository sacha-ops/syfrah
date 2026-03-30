//! `syfrah vpc create|list|delete` handlers.

use anyhow::{bail, Context, Result};
use syfrah_state::LayerDb;

use crate::store::OrgStore;
use crate::types::VpcOwner;

const DEFAULT_PROJECT_CIDR: &str = "10.1.0.0/16";
const DEFAULT_SHARED_CIDR: &str = "10.100.0.0/16";

fn open_store() -> Result<OrgStore> {
    let db = LayerDb::open("org").context("failed to open org database")?;
    Ok(OrgStore::new(db))
}

pub fn run_create(
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

    let store = open_store()?;

    if shared {
        let cidr = cidr.unwrap_or(DEFAULT_SHARED_CIDR);
        let vpc = store
            .create_shared_vpc(name, org, cidr)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("VPC created: {}", vpc.name);
        println!("  Org:      {org}");
        println!("  Shared:   yes");
        println!("  CIDR:     {}", vpc.cidr);
        println!("  VNI:      {}", vpc.vni);
        println!("  Created:  {}", format_timestamp(vpc.created_at));
    } else {
        let project = project.unwrap();
        let cidr = cidr.unwrap_or(DEFAULT_PROJECT_CIDR);
        let vpc = store
            .create_vpc(name, org, project, cidr)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("VPC created: {}", vpc.name);
        println!("  Org:      {org}");
        println!("  Project:  {project}");
        println!("  CIDR:     {}", vpc.cidr);
        println!("  VNI:      {}", vpc.vni);
        println!("  Created:  {}", format_timestamp(vpc.created_at));
    }

    Ok(())
}

pub fn run_list(org: Option<&str>, project: Option<&str>, json: bool) -> Result<()> {
    let store = open_store()?;
    let vpcs = store
        .list_vpcs(org, project)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

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
            VpcOwner::Project { org, project } => format!("{org}/{project}"),
            VpcOwner::Org(o) => o.clone(),
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

pub fn run_delete(name: &str, org: &str, yes: bool) -> Result<()> {
    let store = open_store()?;

    // Check it exists first
    match store.get_vpc(org, name) {
        Ok(Some(_)) => {}
        Ok(None) => bail!("vpc '{name}' not found in org '{org}'"),
        Err(e) => bail!("failed to look up vpc: {e}"),
    }

    if !yes {
        eprint!("Delete VPC '{name}' from org '{org}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    store
        .delete_vpc(org, name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("VPC '{name}' deleted.");
    Ok(())
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
        // Project-scoped VPC with all flags
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

        // Shared VPC
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

        // Project-scoped without explicit CIDR
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
        // With filters and --json
        let cmd = parse(&["list", "--project", "backend", "--org", "acme", "--json"]);
        match cmd {
            VpcCommand::List { project, org, json } => {
                assert_eq!(project.as_deref(), Some("backend"));
                assert_eq!(org.as_deref(), Some("acme"));
                assert!(json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        // No filters
        let cmd = parse(&["list"]);
        match cmd {
            VpcCommand::List { project, org, json } => {
                assert!(project.is_none());
                assert!(org.is_none());
                assert!(!json);
            }
            other => panic!("expected List, got {other:?}"),
        }

        // Only org filter
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
    fn vpc_delete_parse() {
        // With --yes
        let cmd = parse(&["delete", "my-vpc", "--org", "acme", "--yes"]);
        match cmd {
            VpcCommand::Delete { name, org, yes } => {
                assert_eq!(name, "my-vpc");
                assert_eq!(org, "acme");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        // Without --yes
        let cmd = parse(&["delete", "my-vpc", "--org", "acme"]);
        match cmd {
            VpcCommand::Delete { name, org, yes } => {
                assert_eq!(name, "my-vpc");
                assert_eq!(org, "acme");
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        // Short -y flag
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
