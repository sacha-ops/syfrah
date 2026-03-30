use clap::Subcommand;

use crate::store::OrgStore;
use crate::types::{now_epoch, Project};

/// Project management commands.
#[derive(Subcommand)]
pub enum ProjectCommand {
    /// Create a new project
    Create {
        /// Project name
        name: String,
        /// Organization name
        #[arg(long)]
        org: String,
    },
    /// List all projects
    List {
        /// Filter by organization
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a project
    Delete {
        /// Project name
        name: String,
        /// Organization name
        #[arg(long)]
        org: String,
    },
}

pub fn run_project_command(cmd: &ProjectCommand) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;

    match cmd {
        ProjectCommand::Create { name, org } => {
            let project = Project {
                id: format!("{org}/{name}"),
                name: name.clone(),
                org_id: org.clone(),
                created_at: now_epoch(),
            };
            store
                .create_project(&project)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Created project '{name}' in org '{org}'");
            Ok(())
        }
        ProjectCommand::List { org } => {
            let projects = store.list_projects().map_err(|e| anyhow::anyhow!("{e}"))?;
            let filtered: Vec<_> = match org {
                Some(o) => projects.into_iter().filter(|p| &p.org_id == o).collect(),
                None => projects,
            };
            if filtered.is_empty() {
                println!("No projects found.");
            } else {
                println!("{:<30} {:<20} {:<20}", "NAME", "ORG", "CREATED");
                for p in &filtered {
                    println!("{:<30} {:<20} {:<20}", p.name, p.org_id, p.created_at);
                }
            }
            Ok(())
        }
        ProjectCommand::Delete { name, org } => {
            let deleted = store
                .delete_project(org, name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if deleted {
                println!("Deleted project '{name}' from org '{org}'");
            } else {
                anyhow::bail!("project '{name}' not found in org '{org}'");
            }
            Ok(())
        }
    }
}
