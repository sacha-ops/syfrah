//! CLI commands for `syfrah org ...` and `syfrah project ...`.

pub mod org;
pub mod project;

use clap::Subcommand;

/// Top-level org CLI command.
#[derive(Debug, Subcommand)]
pub enum OrgCommand {
    /// Create a new organization
    #[command(after_help = "Examples:\n  syfrah org create acme\n  syfrah org create my-company")]
    Create {
        /// Organization name (lowercase alphanumeric and hyphens, 3-63 chars)
        name: String,
    },
    /// List all organizations
    #[command(after_help = "Examples:\n  syfrah org list\n  syfrah org list --json")]
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete an organization
    #[command(after_help = "Examples:\n  syfrah org delete acme\n  syfrah org delete acme --yes")]
    Delete {
        /// Organization name
        name: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
}

/// Top-level project CLI command.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a new project under an organization
    Create {
        /// Project name (lowercase alphanumeric + hyphens, 3-63 chars)
        name: String,
        /// Organization this project belongs to
        #[arg(long)]
        org: String,
    },
    /// List projects
    List {
        /// Filter by organization
        #[arg(long)]
        org: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a project
    Delete {
        /// Project name
        name: String,
        /// Organization the project belongs to
        #[arg(long)]
        org: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

/// Execute an org CLI command.
pub fn run(cmd: OrgCommand) -> anyhow::Result<()> {
    match cmd {
        OrgCommand::Create { name } => org::run_create(name),
        OrgCommand::List { json } => org::run_list(json),
        OrgCommand::Delete { name, yes } => org::run_delete(name, yes),
    }
}

/// Execute a project CLI command.
pub fn run_project(cmd: ProjectCommand) -> anyhow::Result<()> {
    match cmd {
        ProjectCommand::Create { name, org } => project::create(&name, &org),
        ProjectCommand::List { org, json } => project::list(org.as_deref(), json),
        ProjectCommand::Delete { name, org, yes } => project::delete(&name, &org, yes),
    }
}
