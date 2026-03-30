use clap::Subcommand;

use crate::store::OrgStore;
use crate::types::{now_epoch, Org};

/// Organization management commands.
#[derive(Subcommand)]
pub enum OrgCommand {
    /// Create a new organization
    Create {
        /// Organization name
        name: String,
    },
    /// List all organizations
    List,
    /// Delete an organization
    Delete {
        /// Organization name
        name: String,
    },
}

pub fn run_org_command(cmd: &OrgCommand) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;

    match cmd {
        OrgCommand::Create { name } => {
            let org = Org {
                id: name.clone(),
                name: name.clone(),
                created_at: now_epoch(),
            };
            store.create_org(&org).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Created organization '{name}'");
            Ok(())
        }
        OrgCommand::List => {
            let orgs = store.list_orgs().map_err(|e| anyhow::anyhow!("{e}"))?;
            if orgs.is_empty() {
                println!("No organizations found.");
            } else {
                println!("{:<30} {:<20}", "NAME", "CREATED");
                for org in &orgs {
                    println!("{:<30} {:<20}", org.name, org.created_at);
                }
            }
            Ok(())
        }
        OrgCommand::Delete { name } => {
            let deleted = store.delete_org(name).map_err(|e| anyhow::anyhow!("{e}"))?;
            if deleted {
                println!("Deleted organization '{name}'");
            } else {
                anyhow::bail!("organization '{name}' not found");
            }
            Ok(())
        }
    }
}
