//! `syfrah project` subcommands.

use crate::cli::ProjectCommand;
use crate::store::OrgStore;

pub fn run(cmd: ProjectCommand) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("{e}"))?;

    match cmd {
        ProjectCommand::Create { name, org } => {
            let project = store
                .create_project(&name, &org)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "Project '{}' created in organization '{}'.",
                project.name, project.org
            );
            Ok(())
        }
        ProjectCommand::List { org } => {
            let projects = store
                .list_projects(org.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if projects.is_empty() {
                println!("No projects found.");
            } else {
                println!("{:<30} {:<20} {:<20}", "NAME", "ORG", "CREATED");
                for p in projects {
                    println!("{:<30} {:<20} {:<20}", p.name, p.org, p.created_at);
                }
            }
            Ok(())
        }
        ProjectCommand::Delete { name, org } => {
            store
                .delete_project(&name, &org)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Project '{name}' deleted.");
            Ok(())
        }
    }
}
