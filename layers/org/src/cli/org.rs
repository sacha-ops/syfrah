//! `syfrah org` subcommands.

use crate::cli::OrgCommand;
use crate::store::OrgStore;

pub fn run(cmd: OrgCommand) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("{e}"))?;

    match cmd {
        OrgCommand::Create { name } => {
            let org = store
                .create_org(&name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Organization '{}' created.", org.name);
            Ok(())
        }
        OrgCommand::List => {
            let orgs = store.list_orgs().map_err(|e| anyhow::anyhow!("{e}"))?;
            if orgs.is_empty() {
                println!("No organizations found.");
            } else {
                println!("{:<30} {:<20}", "NAME", "CREATED");
                for org in orgs {
                    println!("{:<30} {:<20}", org.name, org.created_at);
                }
            }
            Ok(())
        }
        OrgCommand::Delete { name } => {
            store
                .delete_org(&name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Organization '{name}' deleted.");
            Ok(())
        }
    }
}
