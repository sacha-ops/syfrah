//! Implementation of `syfrah vpc` subcommands.

use anyhow::bail;
use syfrah_state::LayerDb;

use crate::store::OrgStore;
use crate::types::VpcOwner;
use crate::validation::validate_name;

/// Create a VPC.
pub fn run_create(
    name: &str,
    org: &str,
    project: Option<&str>,
    cidr: &str,
    shared: bool,
) -> anyhow::Result<()> {
    if let Err(e) = validate_name(name, "vpc") {
        bail!("{e}");
    }

    let db = LayerDb::open("org").map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;
    let store = OrgStore::new(db);

    let owner = if let Some(proj) = project {
        if shared {
            bail!("shared VPCs must be org-owned, do not pass --project");
        }
        let key = format!("{org}/{proj}");
        VpcOwner::Project(crate::types::ProjectId(key))
    } else {
        VpcOwner::Org(crate::types::OrgId(org.to_string()))
    };

    match store.create_vpc(name, cidr, owner, shared) {
        Ok(vpc) => {
            println!(
                "VPC '{}' created (VNI {}, CIDR {}, shared: {}).",
                vpc.name, vpc.vni, vpc.cidr, vpc.shared
            );
            Ok(())
        }
        Err(crate::error::OrgError::VpcAlreadyExists(_)) => {
            bail!("vpc '{name}' already exists");
        }
        Err(e) => {
            bail!("failed to create vpc: {e}");
        }
    }
}

/// List VPCs.
pub fn run_list(org: Option<&str>, json: bool) -> anyhow::Result<()> {
    let db = LayerDb::open("org").map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;
    let store = OrgStore::new(db);

    let vpcs = store
        .list_vpcs(org)
        .map_err(|e| anyhow::anyhow!("failed to list vpcs: {e}"))?;

    if json {
        let json_str = serde_json::to_string_pretty(&vpcs)?;
        println!("{json_str}");
        return Ok(());
    }

    if vpcs.is_empty() {
        println!("(no VPCs)");
        return Ok(());
    }

    let tw = term_width();
    let header = format!(
        "{:<20} {:<18} {:<8} {:<8} {:<20}",
        "NAME", "CIDR", "VNI", "SHARED", "OWNER"
    );
    if console::Term::stdout().is_term() {
        let truncated = &header[..header.len().min(tw)];
        println!("{}", console::Style::new().bold().apply_to(truncated));
    } else {
        println!("{}", &header[..header.len().min(tw)]);
    }
    println!("{}", "-".repeat(74.min(tw)));

    for vpc in &vpcs {
        let owner_str = match &vpc.owner {
            VpcOwner::Project(id) => format!("project:{}", id.0),
            VpcOwner::Org(id) => format!("org:{}", id.0),
        };
        let shared_str = if vpc.shared { "yes" } else { "no" };
        let row = format!(
            "{:<20} {:<18} {:<8} {:<8} {:<20}",
            vpc.name, vpc.cidr, vpc.vni, shared_str, owner_str
        );
        println!("{}", &row[..row.len().min(tw)]);
    }

    Ok(())
}

/// Delete a VPC.
pub fn run_delete(name: &str, yes: bool) -> anyhow::Result<()> {
    let db = LayerDb::open("org").map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;
    let store = OrgStore::new(db);

    match store.get_vpc(name) {
        Ok(Some(_)) => {}
        Ok(None) => bail!("vpc '{name}' not found"),
        Err(e) => bail!("failed to look up vpc: {e}"),
    }

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

    match store.delete_vpc(name) {
        Ok(()) => {
            println!("VPC '{name}' deleted.");
            Ok(())
        }
        Err(e) => bail!("failed to delete vpc: {e}"),
    }
}

/// Attach a project to a shared VPC.
pub fn run_attach(vpc_name: &str, project: &str) -> anyhow::Result<()> {
    let db = LayerDb::open("org").map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;
    let store = OrgStore::new(db);

    match store.attach_vpc(vpc_name, project) {
        Ok(()) => {
            println!("Project '{project}' attached to VPC '{vpc_name}'.");
            Ok(())
        }
        Err(crate::error::OrgError::VpcNotFound(_)) => {
            bail!("vpc '{vpc_name}' not found");
        }
        Err(crate::error::OrgError::VpcNotShared(_)) => {
            bail!("vpc '{vpc_name}' is not shared; only shared VPCs can be attached");
        }
        Err(crate::error::OrgError::VpcAlreadyAttached { .. }) => {
            bail!("project '{project}' is already attached to vpc '{vpc_name}'");
        }
        Err(e) => bail!("failed to attach: {e}"),
    }
}

/// Detach a project from a shared VPC.
pub fn run_detach(vpc_name: &str, project: &str) -> anyhow::Result<()> {
    let db = LayerDb::open("org").map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;
    let store = OrgStore::new(db);

    match store.detach_vpc(vpc_name, project) {
        Ok(()) => {
            println!("Project '{project}' detached from VPC '{vpc_name}'.");
            Ok(())
        }
        Err(crate::error::OrgError::VpcNotFound(_)) => {
            bail!("vpc '{vpc_name}' not found");
        }
        Err(crate::error::OrgError::VpcNotAttached { .. }) => {
            bail!("project '{project}' is not attached to vpc '{vpc_name}'");
        }
        Err(e) => bail!("failed to detach: {e}"),
    }
}

fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(120)
}
