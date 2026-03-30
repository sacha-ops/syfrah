//! `syfrah subnet create|list|delete` handlers.

use anyhow::{Context, Result};
use syfrah_state::LayerDb;

use crate::store::OrgStore;
use crate::types::EnvironmentId;
use crate::vpc::VpcStore;

fn open_store() -> Result<OrgStore> {
    let db = LayerDb::open("org").context("failed to open org database")?;
    Ok(OrgStore::new(db))
}

fn open_vpc_store() -> Result<VpcStore> {
    let db = LayerDb::open("org").context("failed to open org database")?;
    Ok(VpcStore::new(db))
}

pub fn run_create(
    name: &str,
    env: &str,
    project: &str,
    org: &str,
    vpc: Option<&str>,
    cidr: Option<&str>,
) -> Result<()> {
    // Resolve VPC name: explicit or default for project.
    // We must ensure the default VPC exists before creating the subnet.
    // Since redb holds a file lock, we must close VpcStore before opening OrgStore.
    let vpc_name = match vpc {
        Some(v) => v.to_string(),
        None => {
            let vpc_store = open_vpc_store()?;
            let default_vpc = vpc_store
                .ensure_default_vpc(org, project)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let name = default_vpc.name.clone();
            drop(vpc_store);
            name
        }
    };

    // Build environment ID
    let env_id = EnvironmentId(format!("{org}/{project}/{env}"));

    let store = open_store()?;
    let subnet = store
        .create_subnet(&vpc_name, &env_id, name, cidr)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Resolve VPC name for display
    let display_vpc = store
        .get_vpc_by_id(&subnet.vpc_id)
        .ok()
        .flatten()
        .map(|v| v.name)
        .unwrap_or_else(|| vpc_name.clone());

    println!("Subnet created: {}", subnet.name);
    println!("  VPC:      {display_vpc}");
    println!("  Env:      {env}");
    println!("  CIDR:     {}", subnet.cidr);
    println!("  Gateway:  {}", subnet.gateway);
    println!("  Created:  {}", format_timestamp(subnet.created_at));

    Ok(())
}

pub fn run_list(
    env: Option<&str>,
    vpc: Option<&str>,
    project: Option<&str>,
    org: Option<&str>,
    json: bool,
) -> Result<()> {
    let store = open_store()?;

    let subnets = if let Some(vpc_name) = vpc {
        store
            .list_subnets(vpc_name)
            .map_err(|e| anyhow::anyhow!("{e}"))?
    } else if let (Some(env_name), Some(proj), Some(org_name)) = (env, project, org) {
        let env_id = EnvironmentId(format!("{org_name}/{proj}/{env_name}"));
        store
            .list_subnets_by_env(&env_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        anyhow::bail!("specify --vpc or --env/--project/--org to list subnets");
    };

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
        let vpc_name = store
            .get_vpc_by_id(&subnet.vpc_id)
            .ok()
            .flatten()
            .map(|v| v.name)
            .unwrap_or_else(|| subnet.vpc_id.0.clone());

        let env_name = subnet
            .env_id
            .0
            .split('/')
            .nth(2)
            .unwrap_or(&subnet.env_id.0);

        println!(
            "{:<20} {:<25} {:<15} {:<18} {:<16} {}",
            subnet.name,
            vpc_name,
            env_name,
            subnet.cidr,
            subnet.gateway,
            format_timestamp(subnet.created_at),
        );
    }

    Ok(())
}

pub fn run_delete(name: &str, vpc: Option<&str>, yes: bool) -> Result<()> {
    let store = open_store()?;

    // Resolve VPC: if provided use it directly, otherwise search all VPCs.
    let vpc_name = match vpc {
        Some(v) => v.to_string(),
        None => {
            let matches = store
                .find_subnets_by_name(name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            match matches.len() {
                0 => anyhow::bail!("subnet '{name}' not found"),
                1 => matches.into_iter().next().unwrap().0,
                _ => {
                    let vpc_names: Vec<String> = matches.into_iter().map(|(v, _)| v).collect();
                    anyhow::bail!(
                        "subnet '{name}' exists in multiple VPCs: {}. Specify --vpc",
                        vpc_names.join(", ")
                    );
                }
            }
        }
    };

    if !yes {
        eprint!("Delete subnet '{name}' from VPC '{vpc_name}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    store
        .delete_subnet(&vpc_name, name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Subnet '{name}' deleted from VPC '{vpc_name}'.");
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
        // All flags
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

        // Without optional flags (vpc and cidr omitted)
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
        // With all filters
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

        // No filters
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

        // Partial filters
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
        // With --vpc and --yes
        let cmd = parse(&["delete", "frontend", "--vpc", "my-vpc", "--yes"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        // Without --yes
        let cmd = parse(&["delete", "frontend", "--vpc", "my-vpc"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        // Short -y flag
        let cmd = parse(&["delete", "-y", "frontend", "--vpc", "my-vpc"]);
        match cmd {
            SubnetCommand::Delete { name, vpc, yes } => {
                assert_eq!(name, "frontend");
                assert_eq!(vpc.as_deref(), Some("my-vpc"));
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }

        // Without --vpc (auto-resolve mode)
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
